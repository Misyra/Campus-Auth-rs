//! 系统托盘模块：图标加载、菜单构建、事件转发与异步泵任务
//!
//! 设计要点：
//! - 托盘图标与菜单事件循环运行在**独立的 OS 线程**上（Windows / Linux，
//!   `tray-icon` 要求）；**macOS 例外**——AppKit 要求托盘/菜单构建与事件分发
//!   都在进程主线程，OS 线程在该平台退化为命令代理，AppKit 调用经 GCD 主队列
//!   投递到主线程（主线程由 `main.rs` 的 macOS 分支交给 NSApplication 事件循环，
//!   2026-11-21 托盘启用架构批）。
//! - OS 线程只负责构建菜单、显示图标（macOS 为投递代理），并把菜单选择通过
//!   通道转发为 [`TrayAction`]，所有真正的业务逻辑（Engine 命令派发、更新检查、
//!   打开浏览器）都在 tokio 泵任务中完成，保持 OS 线程极简。
//! - 退出时由泵任务发送 [`crate::engine::EngineCommand::Shutdown`]，随后通知 OS 线程退出
//!   并 join。

use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use tokio::sync::mpsc;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::app::{self, AxumServeHandle};
use crate::config::{ConfigService, ProfileService};
use crate::container::ServiceContainer;
use crate::engine::{EngineCommand, EngineSlot};
use crate::status::{
    EngineState, LoginSource, LoginStatus, NetworkStatus, StatusManager, StatusSnapshot,
};
use crate::web::state::LogEntry;

/// 托盘图标默认尺寸（生成回退图标时使用）
const FALLBACK_ICON_SIZE: u32 = 32;
/// 运行时图标颜色（绿色，表示运行中）——仅解码失败兜底色块使用
const ACTIVE_COLOR: [u8; 3] = [80, 200, 120];
/// 动作通道容量
const ACTION_CHANNEL_CAPACITY: usize = 64;

/// 菜单构建结果类型：`(Menu, 顶层菜单项容器, 状态项, 监测切换项)`
///
/// 后两个 MenuItem 是 `menu_items` 内对应元素的克隆（MenuItem 内部为 Rc，clone 廉价），
/// 单独返回以便状态变化时调用 `set_text` 动态改写文本。
type MenuBuildResult = (Menu, Vec<Box<dyn IsMenuItem>>, MenuItem, MenuItem);

/// OS 线程内部命令（泵任务 / Drop → OS 线程），单通道承载退出与托盘刷新。
///
/// `TrayIcon` 内部包含 `Rc<RefCell>` 因而不满足 `Send`，必须由 OS 线程独占持有，
/// 故状态驱动的 tooltip/图标更新也只能在 OS 线程内完成，泵任务仅通过本命令转发请求。
enum OsCommand {
    /// 退出 OS 线程并清理
    Quit,
    /// 依据最新状态刷新 tooltip / 图标
    RefreshTray,
}

/// 菜单选择转换后的托盘动作，由 OS 线程产生、被泵任务消费
#[derive(Debug, Clone)]
pub enum TrayAction {
    /// 启动监测（EngineCommand::Start）
    StartMonitor,
    /// 停止监测（EngineCommand::Stop）
    StopMonitor,
    /// 打开 Web 控制台（open::that，URL 携带 `#auth=<bootstrap>` fragment）
    OpenWeb,
    /// 复制带 auth fragment 的界面完整 URL 到系统剪贴板
    CopyWebLink,
    /// 手动触发一次登录（等价于控制台「手动登录」按钮，来源标记 Manual）
    ManualLogin,
    /// 退出（EngineCommand::Shutdown + 停止自身）
    Quit,
}

pub use crate::ServiceHandle;

/// 托盘业务依赖集合（构造时一次性注入，避免函数参数过多触发 clippy `too_many_arguments`）
#[derive(Clone)]
pub struct TrayDeps {
    /// 配置服务（读取端口、Profile 列表等）
    pub config: Arc<ConfigService>,
    /// 状态管理器（订阅状态以更新 tooltip/图标，并取活跃 Profile）
    pub status: Arc<StatusManager>,
    /// 引擎句柄槽（派发命令到「当前活跃」Engine，崩溃重启后自动指向新实例）
    pub engine: EngineSlot,
    /// 登录编排器（托盘「手动登录」入口复用与 Web API 相同的提交路径）
    pub login: Arc<dyn crate::login::LoginApi>,
    /// Profile 服务（枚举 Profile 构建子菜单）
    pub profile_service: Arc<ProfileService>,
    /// 服务容器（轻量模式按需启动 Axum 时需要）
    pub container: Arc<ServiceContainer>,
    /// 日志广播通道（按需启动 Axum 时需要）
    pub log_tx: broadcast::Sender<LogEntry>,
    /// 默认监听端口（Axum 未运行时回退使用）
    pub port: u16,
    /// 绑定地址（按需启动 Axum 时使用）
    pub host: Option<String>,
    /// 是否运行在轻量模式（Axum 按需启动）
    pub lightweight: bool,
    /// 管理密码覆盖值（CLI/env，透传给按需启动的 Axum 与凭据门禁）
    pub management_password_override: Option<String>,
    /// 显式豁免无凭据的非回环绑定（--allow-unauthenticated-bind，透传给凭据门禁）
    pub allow_unauthenticated_bind: bool,
    /// 应用级关闭令牌（Quit 时取消，驱动 launcher 走完整优雅关闭流程）
    pub shutdown: CancellationToken,
}

/// 托盘管理器：整体持有 [`TrayDeps`]（单一字段，不再逐字段镜像复制），
/// 另持有自身专属的跨线程通道，负责创建并驱动托盘
pub struct TrayManager {
    /// 业务依赖集合（spawn 时 clone 移入泵任务）
    deps: TrayDeps,

    /// 菜单动作发送端（OS 线程 → 泵任务）
    action_tx: mpsc::Sender<TrayAction>,
    /// 菜单动作接收端（在 spawn 时取出，移入泵任务）
    action_rx: Mutex<Option<mpsc::Receiver<TrayAction>>>,
    /// OS 线程命令发送端（泵任务/Drop → OS 线程），承载退出与托盘刷新
    os_cmd_tx: std_mpsc::Sender<OsCommand>,
    /// OS 线程命令接收端（在 spawn 时取出，移入 OS 线程）
    os_cmd_rx: Mutex<Option<std_mpsc::Receiver<OsCommand>>>,
    /// OS 托盘线程 JoinHandle（take 后仅一处 join，避免重复 join）
    os_join: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl TrayManager {
    /// 构造托盘管理器。
    ///
    /// 仅保存依赖与通道；真正的图标与菜单在 [`spawn`](TrayManager::spawn) 中于专用 OS 线程上构建。
    pub fn new(deps: TrayDeps) -> Arc<Self> {
        let (action_tx, action_rx) = mpsc::channel(ACTION_CHANNEL_CAPACITY);
        let (os_cmd_tx, os_cmd_rx) = std_mpsc::channel();
        Arc::new(Self {
            deps,
            action_tx,
            action_rx: Mutex::new(Some(action_rx)),
            os_cmd_tx,
            os_cmd_rx: Mutex::new(Some(os_cmd_rx)),
            os_join: Arc::new(Mutex::new(None)),
        })
    }

    /// 启动托盘：在专用 OS 线程上构建图标与菜单，并 spawn 一个 tokio 泵任务消费 [`TrayAction`]。
    ///
    /// 返回 [`ServiceHandle`]，调用其 [`ServiceHandle::stop`] 可优雅停止并 join OS 线程。
    ///
    /// **平台事件循环分工**（三平台各自的线程约束，收敛于 [`run_os_thread`]）：
    /// - Windows：托盘直接构建在专用 OS 线程，线程内 pump 消息循环
    /// - Linux：构建前 `gtk::init`，线程内持续 gtk 主循环
    /// - macOS：**专用线程只作命令代理**——AppKit 要求托盘/菜单构建与事件分发
    ///   都在进程主线程（tray-icon 非主线程构建返回 `NotMainThread`，muda 的
    ///   `Menu::new` 在非主线程直接 panic），故构建、动态更新与清理一律经
    ///   GCD 主队列投递到主线程执行（主线程由 `main.rs` 的 macOS 分支交给
    ///   `NSApplication` 事件循环，2026-11-21 托盘启用架构批）
    pub fn spawn(self: &Arc<Self>) -> ServiceHandle {
        let action_rx = self
            .action_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("spawn 仅可被调用一次");
        let os_cmd_rx = self
            .os_cmd_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("spawn 仅可被调用一次");

        // 跨线程共享的数据（OS 线程独占持有 TrayIcon，仅取所需 Arc）
        let status = Arc::clone(&self.deps.status);
        let status_for_pump = status.clone();
        let action_tx = self.action_tx.clone();
        let os_cmd_tx_pump = self.os_cmd_tx.clone();
        let os_join = Arc::clone(&self.os_join);
        // 泵任务需要的全部业务依赖：整体 clone 一份（不再逐字段手工重建镜像）
        let deps = self.deps.clone();
        // 轻量模式下按需启动的 Axum 句柄（仅在该模式首次「打开控制台」时创建）
        let axum_handle: Arc<Mutex<Option<AxumServeHandle>>> = Arc::new(Mutex::new(None));

        // ---- OS 托盘线程：构建菜单与图标，独占持有 TrayIcon，等待命令 ----
        let os_handle = thread::spawn(move || {
            run_os_thread(status, action_tx, os_cmd_rx);
        });

        // 记录 OS 线程句柄，供泵任务或 Drop 在结束时 join
        *self.os_join.lock().unwrap_or_else(|e| e.into_inner()) = Some(os_handle);

        // ---- tokio 泵任务：消费 TrayAction 并派发到 Engine ----
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let pump = tokio::spawn(async move {
            let mut status_rx = status_for_pump.subscribe();
            let mut action_rx = action_rx;
            // 去重键：仅当影响 tooltip/图标/菜单文本的字段变化时才请求刷新，
            // 避免 uptime 等高频无关字段触发冗余的 OS 线程刷新（历史遗留 #10）。
            let mut last_key: Option<(EngineState, NetworkStatus, LoginStatus, Option<String>)> =
                None;

            loop {
                tokio::select! {
                    // 外部停止信号（ServiceHandle::stop 或 Drop 导致发送端关闭）
                    stop_res = stop_rx.changed() => {
                        if stop_res.is_err() || *stop_rx.borrow() {
                            break;
                        }
                    }
                    // 菜单动作
                    action = action_rx.recv() => {
                        match action {
                            Some(action) => {
                                let quit = handle_action(
                                    &action,
                                    &deps,
                                    &axum_handle,
                                )
                                .await;
                                if quit {
                                    break;
                                }
                            }
                            None => break, // 发送端已丢弃
                        }
                    }
                    // 状态变化 → 仅当展示相关字段变化才请求 OS 线程刷新 tooltip/图标
                    Ok(()) = status_rx.changed() => {
                        let snap = status_rx.borrow_and_update();
                        let key = (
                            snap.engine_state,
                            snap.network_status,
                            snap.login_status,
                            snap.login_message.clone(),
                        );
                        if last_key.as_ref() != Some(&key) {
                            last_key = Some(key);
                            if let Err(e) = os_cmd_tx_pump.send(OsCommand::RefreshTray) {
                                debug!("托盘刷新请求发送失败（OS 线程可能已退出）: {e}");
                            }
                        }
                    }
                }
            }

            // 通知并 join OS 托盘线程
            let _ = os_cmd_tx_pump.send(OsCommand::Quit);
            if let Some(h) = os_join.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = h.join();
            }
            debug!("托盘泵任务退出");
        });

        ServiceHandle {
            stop_tx,
            join_handle: pump,
            name: "tray",
        }
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        // 若泵任务尚未触发退出，这里确保 OS 线程被通知并 join
        let _ = self.os_cmd_tx.send(OsCommand::Quit);
        if let Some(h) = self
            .os_join
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = h.join();
        }
    }
}

/// OS 托盘线程体：构建菜单与图标，独占持有 [`TrayIcon`]，等待命令。
///
/// 从 [`TrayManager::spawn`] 抽出——原 180 行闭包混杂资源构建、事件注册与
/// 三个平台的消息循环。资源构建失败（gtk/图标）直接返回，调用方无需处理。
fn run_os_thread(
    status: Arc<StatusManager>,
    action_tx: mpsc::Sender<TrayAction>,
    os_cmd_rx: std_mpsc::Receiver<OsCommand>,
) {
    // macOS：本线程不执行任何 AppKit 调用，只作命令代理——托盘构建/动态
    // 更新/清理全部经 GCD 主队列投递到进程主线程执行（AppKit 线程模型与
    // 三平台分工见 TrayManager::spawn 的注释；muda 的 Menu::new 在非主
    // 线程直接 panic，是原 macOS 禁用的根因之一）。构建与事件循环在此
    // 结束，Windows/Linux 的直接构建路径在 macOS 构建下整体编译剔除
    #[cfg(target_os = "macos")]
    {
        run_macos_proxy_loop(status, action_tx, os_cmd_rx);
        debug!("系统托盘线程退出（macOS 代理路径）");
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Linux：tray-icon 后端基于 gtk/libappindicator，构建图标前必须完成
        // gtk::init，且 gtk 主循环要在同一线程持续运行（见下方 gtk::main）——
        // 否则菜单/图标事件永远不会分发，托盘完全无响应
        #[cfg(target_os = "linux")]
        if let Err(e) = gtk::init() {
            error!("gtk 初始化失败，托盘不可用: {e}");
            return;
        }

        // 菜单项对象必须比 Menu/TrayIcon 存活更久（muda 内部持有引用）
        // menu_items 绑定本身保持对象存活到线程结束；status_item / toggle_item 用于动态改文本
        let (menu, menu_items, status_item, toggle_item) = build_menu();
        let _ = &menu_items;
        // 首次按当前状态设置文本与状态行强调态
        let first_snapshot = status.borrow();
        status_item.set_text(status_menu_label(&first_snapshot));
        if status_item.is_enabled() != status_item_enabled(first_snapshot.engine_state) {
            status_item.set_enabled(status_item_enabled(first_snapshot.engine_state));
        }
        toggle_item.set_text(monitor_toggle_label(first_snapshot.engine_state));
        drop(first_snapshot);

        // 加载图标（缺失则回退到生成色块），并准备运行/停止两种图标：
        // 停止态用同一 logo 的灰色版保持品牌识别（此前是纯红色块，无信息量）
        let (rgba, w, h) = load_tray_rgba();
        let active_icon = make_icon(rgba.clone(), w, h);
        let mut inactive_rgba = rgba;
        for px in inactive_rgba.chunks_mut(4) {
            px[0] = 150;
            px[1] = 150;
            px[2] = 150;
        }
        let inactive_icon = make_icon(inactive_rgba, w, h);

        let built = match active_icon.as_ref() {
            Some(icon) => TrayIconBuilder::new()
                .with_icon(icon.clone())
                .with_menu(Box::new(menu))
                .with_tooltip("认证喵 Campus-Auth")
                .build(),
            None => {
                error!("无法加载或生成托盘图标");
                return;
            }
        };

        let tray = match built {
            Ok(t) => {
                info!("系统托盘已创建");
                Rc::new(t)
            }
            Err(e) => {
                error!("系统托盘创建失败: {e}");
                return;
            }
        };

        // 注册全局菜单事件处理器：转发为 TrayAction（不阻塞 OS 线程）
        let action_tx_tray = action_tx.clone();
        let status_for_menu = Arc::clone(&status);
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id: String = event.id().0.clone();
            // monitor_toggle 的具体动作（启动/停止）由当前引擎状态决定，
            // 这样菜单项 id 固定，文本可随状态切换
            if let Some(action) = menu_action_for(id.as_str(), &status_for_menu.borrow()) {
                // OS 线程用 try_send（非阻塞）转发；通道满/关闭时丢弃
                if action_tx.try_send(action).is_err() {
                    warn!("托盘泵任务已退出，丢弃菜单事件");
                }
            }
        }));

        // 注册全局托盘图标事件处理器：左键单击打开 Web 控制台
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
                && button == tray_icon::MouseButton::Left
                && button_state == tray_icon::MouseButtonState::Up
                && let Err(e) = action_tx_tray.try_send(TrayAction::OpenWeb)
            {
                debug!("托盘点击事件转发失败（通道满或泵任务已退出）: {e}");
            }
        }));

        // 阻塞等待泵任务 / Drop 发来的命令（退出或刷新托盘），保持线程存活。
        // 按值移交所有权（Linux timeout_add_local 闭包要求 'static，见 run_os_event_loop）。
        run_os_event_loop(
            tray,
            status,
            active_icon,
            inactive_icon,
            status_item,
            toggle_item,
            os_cmd_rx,
        );

        // 清理：清除全局处理器（释放 action_tx 引用）
        MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
        TrayIconEvent::set_event_handler::<fn(TrayIconEvent)>(None);
        debug!("系统托盘线程退出");
    }
}

// ============================================================
// macOS 平台实现：主线程经 GCD 主队列投递，OS 线程仅作命令代理
// ============================================================

/// macOS 侧托盘状态（主线程独占持有，不满足 `Send`）
///
/// `TrayIcon` / `MenuItem` / `Menu` 内部为 `Rc`，跨线程移动即违反线程安全，
/// 故常驻主线程、经 [`MAC_TRAY_STATE`]（主线程 thread_local）存取；主队列
/// 闭包一律只捕获 `Send` 数据（`Arc<StatusManager>` / 动作通道两端），非
/// Send 的托盘对象全部经 thread_local 就地访问
#[cfg(target_os = "macos")]
struct MacTrayState {
    /// 托盘图标句柄（drop 即摘除菜单栏图标，必须与进程同寿命）
    tray: Rc<TrayIcon>,
    /// 顶层菜单项容器（muda 内部持引用，须与 Menu/TrayIcon 同寿命——
    /// 字段仅承担保命职责、不做业务读取，dead_code 豁免即为此）
    #[allow(dead_code)]
    menu_items: Vec<Box<dyn IsMenuItem>>,
    /// 运行态图标（绿色）
    active_icon: Option<Icon>,
    /// 停止态图标（灰色）
    inactive_icon: Option<Icon>,
    /// 「状态」信息行（动态改文本）
    status_item: MenuItem,
    /// 「启动/停止监测」切换项（动态改文本）
    toggle_item: MenuItem,
}

// 主线程托盘状态存取点（仅经主队列闭包读写）。
// 主队列闭包执行于进程主线程，`thread_local!` 在此处解析到的就是主线程
// 实例；`const` 初始化避免首次访问的懒初始化开销。（`thread_local!` 宏
// 展开不支持 `///` 文档注释，故用行内注释）
#[cfg(target_os = "macos")]
thread_local! {
    static MAC_TRAY_STATE: std::cell::RefCell<Option<MacTrayState>> =
        const { std::cell::RefCell::new(None) };
}

/// macOS 专用 OS 线程体：命令代理
///
/// 把泵任务 / Drop 发来的 [`OsCommand`] 翻译成主队列投递：
/// - 构建（一次性）与刷新经 `dispatch_async`（无需回执，主队列串行保序）
/// - 退出清理同样异步投递——主队列 FIFO 保证清理块先于 `main.rs` 优雅关闭
///   尾部投递的 `NSApp.stop` 块执行，托盘资源不会滞留到进程终结
///
/// 本线程自身不触碰任何 AppKit 对象
#[cfg(target_os = "macos")]
fn run_macos_proxy_loop(
    status: Arc<StatusManager>,
    action_tx: mpsc::Sender<TrayAction>,
    os_cmd_rx: std_mpsc::Receiver<OsCommand>,
) {
    use dispatch2::DispatchQueue;

    // 构建投递主队列：NSApplication 事件循环开始排水后执行（main.rs 的
    // macOS 分支先 sharedApplication 再进入 run，本块必然晚于应用实例创建，
    // 无「run loop 未启动」竞态——投递块在 run() 排水主队列时自然执行）
    let status_for_build = status.clone();
    DispatchQueue::main().exec_async(move || {
        build_mac_tray_on_main(status_for_build, action_tx);
    });

    loop {
        match os_cmd_rx.recv() {
            Ok(OsCommand::Quit) => {
                DispatchQueue::main().exec_async(clear_mac_tray_on_main);
                break;
            }
            Ok(OsCommand::RefreshTray) => {
                let status = status.clone();
                DispatchQueue::main().exec_async(move || {
                    MAC_TRAY_STATE.with(|cell| {
                        let state = cell.borrow();
                        let Some(st) = state.as_ref() else {
                            return;
                        };
                        // StatusManager::borrow 直接返回快照值
                        let snap = status.borrow();
                        update_tray(
                            &st.tray,
                            &snap,
                            &st.active_icon,
                            &st.inactive_icon,
                            &st.status_item,
                            &st.toggle_item,
                        );
                    });
                });
            }
            // 发送端已丢弃（泵任务退出后 Drop 路径）：与 Quit 同清理
            Err(_) => {
                DispatchQueue::main().exec_async(clear_mac_tray_on_main);
                break;
            }
        }
    }
    debug!("macOS 托盘代理线程退出");
}

/// macOS 主线程托盘构建（仅在主队列闭包内执行）
///
/// 与 Windows/Linux 在 OS 线程的直接构建**有意保持重复**而非抽公共函数：
/// 三平台的构建时序约束各不相同（gtk::init / 消息循环 pump / 主队列），
/// 抽象收敛会让行为稳定的另外两平台承担 macOS 架构改造的回归风险；
/// 两份构建的字段与文案由既有单测（`monitor_toggle_label` 等）钉住一致性
#[cfg(target_os = "macos")]
fn build_mac_tray_on_main(status: Arc<StatusManager>, action_tx: mpsc::Sender<TrayAction>) {
    // muda 的 Menu/MenuItem 在 macOS 要求主线程（非主线程 Menu::new 直接
    // panic）——本函数只经主队列投递调用
    let (menu, menu_items, status_item, toggle_item) = build_menu();
    let first_snapshot = status.borrow();
    status_item.set_text(status_menu_label(&first_snapshot));
    if status_item.is_enabled() != status_item_enabled(first_snapshot.engine_state) {
        status_item.set_enabled(status_item_enabled(first_snapshot.engine_state));
    }
    toggle_item.set_text(monitor_toggle_label(first_snapshot.engine_state));
    drop(first_snapshot);

    // 加载图标（缺失则回退到生成色块），准备运行/停止两种形态
    let (rgba, w, h) = load_tray_rgba();
    let active_icon = make_icon(rgba.clone(), w, h);
    let mut inactive_rgba = rgba;
    for px in inactive_rgba.chunks_mut(4) {
        px[0] = 150;
        px[1] = 150;
        px[2] = 150;
    }
    let inactive_icon = make_icon(inactive_rgba, w, h);

    let built = match active_icon.as_ref() {
        Some(icon) => TrayIconBuilder::new()
            .with_icon(icon.clone())
            .with_menu(Box::new(menu))
            .with_tooltip("认证喵 Campus-Auth")
            .build(),
        None => {
            error!("无法加载或生成托盘图标");
            return;
        }
    };
    let tray = match built {
        Ok(t) => {
            info!("系统托盘已创建（macOS 主队列）");
            Rc::new(t)
        }
        Err(e) => {
            error!("系统托盘创建失败: {e}");
            return;
        }
    };

    // 全局事件处理器注册（闭包只捕获 Send 数据，转发动作到 tokio 泵通道，
    // 与 Windows/Linux 同一注册点语义）。AppKit 在主线程分发菜单/图标事件，
    // try_send 非阻塞，不拖慢事件循环
    let action_tx_tray = action_tx.clone();
    let status_for_menu = Arc::clone(&status);
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let id: String = event.id().0.clone();
        // monitor_toggle 的具体动作（启动/停止）由当前引擎状态决定；
        // try_send 非阻塞转发，通道满/关闭时丢弃
        if let Some(action) = menu_action_for(id.as_str(), &status_for_menu.borrow())
            && action_tx.try_send(action).is_err()
        {
            warn!("托盘泵任务已退出，丢弃菜单事件");
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button,
            button_state,
            ..
        } = event
            && button == tray_icon::MouseButton::Left
            && button_state == tray_icon::MouseButtonState::Up
            && let Err(e) = action_tx_tray.try_send(TrayAction::OpenWeb)
        {
            debug!("托盘点击事件转发失败（通道满或泵任务已退出）: {e}");
        }
    }));

    // 状态入库：托盘对象自此常驻主线程 thread_local（drop TrayIcon 即摘除
    // 菜单栏图标，故存活期必须覆盖到进程退出前的清理投递）
    MAC_TRAY_STATE.with(|cell| {
        *cell.borrow_mut() = Some(MacTrayState {
            tray,
            menu_items,
            active_icon,
            inactive_icon,
            status_item,
            toggle_item,
        });
    });
}

/// macOS 主线程托盘清理（仅在主队列闭包内执行）
///
/// drop [`MacTrayState`]（AppKit 随之摘除菜单栏图标）并清除全局事件
/// 处理器——与 Windows/Linux 线程退出路径的清理同口径
#[cfg(target_os = "macos")]
fn clear_mac_tray_on_main() {
    MAC_TRAY_STATE.with(|cell| {
        *cell.borrow_mut() = None;
    });
    MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
    TrayIconEvent::set_event_handler::<fn(TrayIconEvent)>(None);
    debug!("macOS 托盘已清理");
}

/// 菜单 id → [`TrayAction`] 纯映射（monitor_toggle 依当前引擎状态二选一）。
///
/// 未知 id 返回 `None`（调用方静默丢弃）；`status_display` 是禁用信息行，
/// 正常不会产生事件，此处同样不映射。
fn menu_action_for(id: &str, snap: &StatusSnapshot) -> Option<TrayAction> {
    match id {
        "monitor_toggle" => {
            if snap.engine_state == EngineState::Running {
                Some(TrayAction::StopMonitor)
            } else {
                Some(TrayAction::StartMonitor)
            }
        }
        "manual_login" => Some(TrayAction::ManualLogin),
        "open_web" => Some(TrayAction::OpenWeb),
        "copy_web_link" => Some(TrayAction::CopyWebLink),
        "quit" => Some(TrayAction::Quit),
        _ => None,
    }
}

/// OS 线程事件循环（平台三分）：阻塞等待退出/刷新命令，保持线程存活。
///
/// 参数一律按值传递（`Rc`/`Arc`/`MenuItem` 均为廉价 clone）：Linux 的
/// `timeout_add_local` 闭包要求 `'static`，传引用会导致 E0521（D 项重构曾
/// 因此仅 Linux 编译失败，Windows 本地无法发现——跨平台改动必须过 unix job）。
/// macOS 不经本函数：代理线程直接驱动主队列（见 [`run_macos_proxy_loop`]），
/// 整体不参与编译，避免参数在 macOS 构建下集体未使用
#[cfg(not(target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
fn run_os_event_loop(
    tray: Rc<TrayIcon>,
    status: Arc<StatusManager>,
    active_icon: Option<Icon>,
    inactive_icon: Option<Icon>,
    status_item: MenuItem,
    toggle_item: MenuItem,
    os_cmd_rx: std_mpsc::Receiver<OsCommand>,
) {
    // Windows 平台必须 pump 消息循环：tray-icon 内部为托盘创建隐藏窗口，
    // 窗口过程处理 WM_USER_TRAYICON 后通过 TrayIconEvent::send 分发到全局
    // handler。若没有 GetMessage/PeekMessage + DispatchMessage，窗口消息不
    // 被处理，托盘左键点击与菜单事件都不会触发。
    #[cfg(windows)]
    run_windows_loop(
        &tray,
        &status,
        &active_icon,
        &inactive_icon,
        &status_item,
        &toggle_item,
        os_cmd_rx,
    );
    // gtk 主循环驱动事件分发；以 50ms 轮询命令通道兼顾刷新与退出，
    // 避免引入跨线程唤醒 glib 主循环的复杂度
    #[cfg(target_os = "linux")]
    run_linux_loop(
        tray,
        status,
        active_icon,
        inactive_icon,
        status_item,
        toggle_item,
        os_cmd_rx,
    );
    // 其余平台（macOS 之外的假想桌面目标）：macOS 已有专属主队列分支，
    // 此处仅保留编译完备性兜底——三平台之外无已知构建目标，且 TrayIcon
    // 构建在这类平台会以 Err 优雅失败；仅保持线程等待退出命令
    #[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
    run_fallback_loop(
        tray,
        status,
        active_icon,
        inactive_icon,
        status_item,
        toggle_item,
        os_cmd_rx,
    );
}

/// Windows 事件循环：pump 消息 + 50ms 命令轮询
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn run_windows_loop(
    tray: &TrayIcon,
    status: &StatusManager,
    active_icon: &Option<Icon>,
    inactive_icon: &Option<Icon>,
    status_item: &MenuItem,
    toggle_item: &MenuItem,
    os_cmd_rx: std_mpsc::Receiver<OsCommand>,
) {
    use std::time::Duration;
    loop {
        pump_windows_messages();
        match os_cmd_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(OsCommand::Quit) => break,
            Ok(OsCommand::RefreshTray) => {
                update_tray(
                    tray,
                    &status.borrow(),
                    active_icon,
                    inactive_icon,
                    status_item,
                    toggle_item,
                );
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Linux 事件循环：gtk 主循环 + 50ms try_recv 轮询
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn run_linux_loop(
    tray: Rc<TrayIcon>,
    status: Arc<StatusManager>,
    active_icon: Option<Icon>,
    inactive_icon: Option<Icon>,
    status_item: MenuItem,
    toggle_item: MenuItem,
    os_cmd_rx: std_mpsc::Receiver<OsCommand>,
) {
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        match os_cmd_rx.try_recv() {
            Ok(OsCommand::Quit) => {
                gtk::main_quit();
                return gtk::glib::ControlFlow::Break;
            }
            Ok(OsCommand::RefreshTray) => {
                update_tray(
                    &tray,
                    &status.borrow(),
                    &active_icon,
                    &inactive_icon,
                    &status_item,
                    &toggle_item,
                );
            }
            Err(std_mpsc::TryRecvError::Empty) => {}
            Err(std_mpsc::TryRecvError::Disconnected) => {
                // 发送端已丢弃，结束线程
                gtk::main_quit();
                return gtk::glib::ControlFlow::Break;
            }
        }
        gtk::glib::ControlFlow::Continue
    });
    gtk::main();
}

/// 其余平台事件循环：阻塞等待退出/刷新命令（macOS 之外三平台无主队列
/// 机制的假想兜底，见 `run_os_event_loop` 尾部分支注释）
#[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
#[allow(clippy::too_many_arguments)]
fn run_fallback_loop(
    tray: Rc<TrayIcon>,
    status: Arc<StatusManager>,
    active_icon: Option<Icon>,
    inactive_icon: Option<Icon>,
    status_item: MenuItem,
    toggle_item: MenuItem,
    os_cmd_rx: std_mpsc::Receiver<OsCommand>,
) {
    loop {
        match os_cmd_rx.recv() {
            Ok(OsCommand::Quit) => break,
            Ok(OsCommand::RefreshTray) => {
                update_tray(
                    &tray,
                    &status.borrow(),
                    &active_icon,
                    &inactive_icon,
                    &status_item,
                    &toggle_item,
                );
            }
            Err(_) => break, // 发送端已丢弃，结束线程
        }
    }
}

/// 处理单个 [`TrayAction`]，派发到 Engine 或执行打开浏览器。
///
/// 返回 `true` 表示应退出托盘（Quit 动作）。
async fn handle_action(
    action: &TrayAction,
    deps: &TrayDeps,
    axum_handle: &Arc<Mutex<Option<AxumServeHandle>>>,
) -> bool {
    match action {
        TrayAction::StartMonitor => {
            if let Err(e) = deps.engine.dispatch(EngineCommand::Start).await {
                warn!("派发 Start 失败: {e:?}");
            }
            false
        }
        TrayAction::StopMonitor => {
            if let Err(e) = deps.engine.dispatch(EngineCommand::Stop).await {
                warn!("派发 Stop 失败: {e:?}");
            }
            false
        }
        TrayAction::OpenWeb => {
            // 轻量模式：Axum 未常驻，首次打开控制台时按需启动。
            // std Mutex 守卫不可跨 await 持有（会破坏泵任务的 Send 约束），
            // 故存在性检查、启动、写回分别短锁
            if deps.lightweight
                && axum_handle
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
            {
                // 凭据门禁参数与完整模式同源：覆盖值来自 CLI/env（TrayDeps 透传），
                // 配置值取实时设置；生效值计算与 exchange 运行期共用同一纯函数，
                // 绑定前的 fail-closed 检查在 prepare_axum_listener 内执行
                let config_password = deps.config.load_settings().global.app.management_password;
                let gate = app::BindCredentialGate {
                    effective_password: crate::web::auth::effective_management_password(
                        deps.management_password_override.as_deref(),
                        &config_password,
                    )
                    .into_owned(),
                    allow_unauthenticated_bind: deps.allow_unauthenticated_bind,
                };
                match app::start_axum(
                    deps.container.clone(),
                    deps.log_tx.clone(),
                    deps.port,
                    deps.host.as_deref(),
                    &gate,
                    deps.management_password_override.clone(),
                    deps.allow_unauthenticated_bind,
                )
                .await
                {
                    Ok(h) => {
                        // 同步实际端口到 `.instance`（PID + PORT），使 --status / --stop
                        // 读到真实端口：初始 record_port 只记录配置端口，自动回退后会失配（历史遗留 M5）
                        write_instance_port(&deps.config.base_path(), h.port);
                        let mut g = axum_handle.lock().unwrap_or_else(|e| e.into_inner());
                        if g.is_none() {
                            *g = Some(h);
                        }
                        info!("托盘按需启动 Axum 成功");
                    }
                    Err(e) => warn!("托盘按需启动 Axum 失败: {e}"),
                }
            }
            // URL 携带 bootstrap fragment（fragment 不进服务器日志）；日志只记
            // 基础 URL，绝不记录 bootstrap 秘钥
            let port = resolve_web_port(deps, axum_handle);
            let bootstrap = crate::web::auth::read_bootstrap_file(&deps.config.base_path());
            let url = crate::web::auth::ui_url(port, bootstrap.as_deref());
            if let Err(e) = open::that(&url) {
                warn!("打开浏览器失败 (http://127.0.0.1:{port}): {e}");
            } else {
                info!("已打开 Web 控制台: http://127.0.0.1:{port}（已携带 auth fragment）");
            }
            false
        }
        TrayAction::CopyWebLink => {
            // 与 OpenWeb 同一 URL 构造口径（含 auth fragment）；剪贴板即用户
            // 显式要求持有该链接，提示中提醒勿外传
            let port = resolve_web_port(deps, axum_handle);
            let bootstrap = crate::web::auth::read_bootstrap_file(&deps.config.base_path());
            let url = crate::web::auth::ui_url(port, bootstrap.as_deref());
            match copy_to_clipboard(&url).await {
                Ok(()) => info!("界面链接已复制到剪贴板（含 auth fragment，请勿外传）"),
                Err(e) => warn!("复制界面链接失败: {e}"),
            }
            false
        }
        TrayAction::ManualLogin => {
            // 与控制台「手动登录」按钮走同一提交路径（LoginApi::submit + Manual 来源），
            // 不另造入口——否则抢占优先级、去重、历史写入等语义会与 Web 侧分叉。
            // 登录可能耗时数十秒（等浏览器/OCR），故 spawn 出去，不阻塞泵任务：
            // 泵任务还负责其它托盘动作，阻塞会让菜单整体失去响应。
            let login = deps.login.clone();
            tokio::spawn(async move {
                let handle = login.submit(LoginSource::Manual, None, None).await;
                let result = handle.await_result().await;
                if result.is_success() {
                    info!("托盘手动登录成功: {}", result.message);
                } else {
                    warn!("托盘手动登录未成功: {}", result.message);
                }
            });
            false
        }
        TrayAction::Quit => {
            info!("收到退出指令，正在关闭应用");
            // 仅派发 Engine Shutdown 不够：launcher 的 wait_for_shutdown 只监听
            // ctrl_c / 关闭令牌 / Web API 信号，进程会继续常驻（且旧逻辑还会
            // 重启 Engine，留下无托盘的僵尸进程）。取消令牌驱动完整优雅关闭。
            deps.shutdown.cancel();
            if let Err(e) = deps.engine.dispatch(EngineCommand::Shutdown).await {
                warn!("派发 Shutdown 失败: {e:?}");
            }
            true
        }
    }
}

/// 根据状态更新托盘 tooltip、图标，以及「状态」行文本与强调色（借启用态表达）
fn update_tray(
    tray: &TrayIcon,
    snap: &StatusSnapshot,
    active: &Option<Icon>,
    inactive: &Option<Icon>,
    status_item: &MenuItem,
    toggle_item: &MenuItem,
) {
    // 「状态」信息行：引擎 · 网络（+ 登录态），随状态刷新。
    // 引擎运行中 → 启用（系统默认深色文字，突出「正在工作」）；
    // 未运行/崩溃 → 禁用（系统灰化，视觉上退到次要）。muda 无颜色 API，见
    // status_item_enabled 的说明。
    status_item.set_text(status_menu_label(snap));
    let running = status_item_enabled(snap.engine_state);
    if status_item.is_enabled() != running {
        status_item.set_enabled(running);
    }
    // 动态切换「启动监测/停止监测」菜单项文本
    toggle_item.set_text(monitor_toggle_label(snap.engine_state));

    // 登录行：成功/失败时附带上次登录结果信息
    let login_line = match (&snap.login_status, &snap.login_message) {
        (LoginStatus::Success | LoginStatus::Failed, Some(msg)) => {
            format!("登录: {} ({})", login_status_str(snap.login_status), msg)
        }
        _ => format!("登录: {}", login_status_str(snap.login_status)),
    };
    let tooltip = format!(
        "认证喵 Campus-Auth\n引擎: {}\n网络: {}\n{}",
        engine_state_str(snap.engine_state),
        network_status_str(snap.network_status),
        login_line,
    );
    if let Err(e) = tray.set_tooltip(Some(tooltip.as_str())) {
        debug!("更新托盘 tooltip 失败: {e}");
    }
    let target = match snap.engine_state {
        EngineState::Running => active,
        EngineState::Stopped | EngineState::Dead => inactive,
    };
    if let Some(icon) = target
        && let Err(e) = tray.set_icon(Some(icon.clone()))
    {
        debug!("更新托盘图标失败: {e}");
    }
}

/// 构建托盘右键菜单。
///
/// 结构（自上而下）：
/// ```text
/// 状态：运行中 · 在线         ← 信息行，随状态刷新；运行中为正常色、未运行灰化
/// ──────────────────────    ← 分隔线：把只读状态与可点操作用视觉分开
/// 停止监测 / 启动监测        ← 随引擎状态切换文本
/// 手动登录
/// 打开控制台
/// 复制界面链接               ← 复制带 auth fragment 的完整 URL（换机/远程场景）
/// 退出
/// ```
///
/// 返回 `(Menu, 顶层菜单项容器, 状态 MenuItem, 监测切换 MenuItem)`：
/// muda 内部持有对菜单项对象的引用，因此 `menu_items` 必须比 [`Menu`]/[`TrayIcon`]
/// 存活更久（由调用方持有）。两个 MenuItem 是 `menu_items` 内的克隆（MenuItem 内部为
/// Rc，clone 廉价），供 [`update_tray`] 在状态变化时 `set_text` 动态改文本。
///
/// 监测切换项使用固定 id `monitor_toggle`，具体动作（启动/停止）由菜单事件 handler
/// 根据当前引擎状态决定，从而实现「id 不变、文本随状态切换」。
///
/// 分隔线以 [`PredefinedMenuItem::separator`] 追加，不参与 id/文本动态更新。
fn build_menu() -> MenuBuildResult {
    let status_item = MenuItem::with_id(
        MenuId::new("status_display"),
        "状态：读取中…",
        // 初始禁用（顶部为「未运行」灰化态），update_tray 会按实际引擎状态复位
        false,
        None,
    );
    let toggle_item = MenuItem::with_id(MenuId::new("monitor_toggle"), "启动检测", true, None);
    let sep = PredefinedMenuItem::separator();
    let menu_items: Vec<Box<dyn IsMenuItem>> = vec![
        Box::new(status_item.clone()),
        Box::new(sep),
        Box::new(toggle_item.clone()),
        Box::new(MenuItem::with_id(
            MenuId::new("manual_login"),
            "手动登录",
            true,
            None,
        )),
        Box::new(MenuItem::with_id(
            MenuId::new("open_web"),
            "打开控制台",
            true,
            None,
        )),
        Box::new(MenuItem::with_id(
            MenuId::new("copy_web_link"),
            "复制界面链接",
            true,
            None,
        )),
        Box::new(MenuItem::with_id(MenuId::new("quit"), "退出", true, None)),
    ];

    let menu_refs: Vec<&dyn IsMenuItem> = menu_items.iter().map(|b| b.as_ref()).collect();
    let menu = Menu::new();
    if let Err(e) = menu.append_items(&menu_refs) {
        error!("托盘菜单项追加失败，菜单可能不完整: {e}");
    }
    (menu, menu_items, status_item, toggle_item)
}

/// 引擎状态 → 「状态」信息行的可点性（借 `set_enabled` 表达视觉强调）。
///
/// muda 的 `MenuItem` 没有颜色/字重 API（仅 `set_text`/`set_enabled`/`set_accelerator`），
/// 故「运行中用深色、未运行用浅色」只能落到启用态：启用走系统默认深色文字，
/// 禁用走系统灰化——这正是 Windows 原生菜单表达「强调 / 次要」的方式。
/// 该行为纯信息行，`menu_action_for` 对 `status_display` 返回 `None`，
/// 故即便处于启用态被点击也不会有任何动作。
fn status_item_enabled(state: EngineState) -> bool {
    state == EngineState::Running
}

/// 引擎/网络状态 → 「状态」行文本。
///
/// 引擎状态是用户最关心的「在不在工作」，网络状态说明「工作对象当前是什么情况」，
/// 故合并为一行展示；登录进行中时追加登录态，避免用户误以为自动登录没反应。
fn status_menu_label(snap: &StatusSnapshot) -> String {
    let login = match snap.login_status {
        LoginStatus::Running => " · 登录中",
        LoginStatus::Success => " · 已登录",
        LoginStatus::Failed => " · 上次失败",
        LoginStatus::Cancelled => " · 已取消",
        LoginStatus::Idle => "",
    };
    format!(
        "状态：{} · {}{}",
        engine_state_str(snap.engine_state),
        network_status_str(snap.network_status),
        login,
    )
}

/// 引擎状态 → 监测切换菜单项文本
fn monitor_toggle_label(state: EngineState) -> &'static str {
    match state {
        EngineState::Running => "停止监测",
        EngineState::Stopped | EngineState::Dead => "启动监测",
    }
}

/// 解析 Web 控制台的实际打开端口：优先运行时端口文件（按需启动 Axum 后写入），
/// 回退到按需启动的 Axum 句柄端口，最后回退默认端口。
///
/// 收敛原 OpenWeb 分支两段冗余回退读端口的样板：对 `axum_handle` 仅加锁一次。
fn resolve_web_port(deps: &TrayDeps, axum_handle: &Arc<Mutex<Option<AxumServeHandle>>>) -> u16 {
    crate::utils::paths::read_runtime_port(&deps.config.base_path())
        .or_else(|| {
            axum_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|h| h.port)
        })
        .unwrap_or(deps.port)
}

/// 把文本写入系统剪贴板（async，供泵任务调用）
///
/// 不引入剪贴板依赖，按平台借用系统工具：Windows `clip`、macOS `pbcopy`、
/// Linux 依次探测 `wl-copy`（Wayland）/ `xclip` / `xsel`。文本一律经 stdin
/// 传入（不进命令行参数，避免进程列表泄露 auth fragment）。
async fn copy_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        clipboard_via(&["clip"], text).await
    }
    #[cfg(target_os = "macos")]
    {
        clipboard_via(&["pbcopy"], text).await
    }
    #[cfg(target_os = "linux")]
    {
        for command in [
            ["wl-copy"],
            ["xclip", "-selection", "clipboard"],
            ["xsel", "--clipboard", "--input"],
        ] {
            if which::which(command[0]).is_ok() {
                return clipboard_via(&command, text).await;
            }
        }
        Err("未找到可用的剪贴板工具（wl-copy / xclip / xsel），请手动复制".to_string())
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        Err("当前平台不支持自动复制剪贴板".to_string())
    }
}

/// 经指定系统工具写剪贴板（`command` 含程序名与参数，文本走 stdin）
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
async fn clipboard_via(command: &[&str], text: &str) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut cmd = tokio::process::Command::new(command[0]);
    cmd.args(&command[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Windows GUI 子系统下避免闪烁控制台窗口（与其他子进程一致）
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动剪贴板工具 {} 失败: {e}", command[0]))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|e| format!("写入剪贴板失败: {e}"))?;
        stdin.shutdown().await.ok();
    }
    let status = child
        .wait()
        .await
        .map_err(|e| format!("等待剪贴板工具退出失败: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("剪贴板工具 {} 退出异常: {status}", command[0]))
    }
}

/// 重写 `.instance` 信息文件（PID + 端口），与 [`crate::utils::lock::InstanceLock::record_port`]
/// 保持同一格式；供轻量模式按需启动 Axum 后校正端口用（历史遗留 M5）。
fn write_instance_port(base_path: &Path, port: u16) {
    let info_path = base_path.join("config").join(".instance");
    let data = format!("{}\n{port}\n", std::process::id());
    // 与 record_port 同样走原子写，避免崩溃残留半截 `.instance` 文件
    if let Err(e) = crate::utils::atomic_write_bytes(&info_path, data.as_bytes()) {
        warn!("更新实例端口信息失败: {e}");
    }
}

/// 托盘图标直接编入二进制：dev 下 exe 在 `target/debug/`、发布期在便携目录，
/// 布局不同；嵌入后不再依赖运行时文件路径（此前 dev 下必现 WARN + 色块回退）。
const TRAY_ICON_PNG: &[u8] = include_bytes!("../../resources/icons/tray.png");

/// 加载托盘图标 RGBA 像素；解码失败时回退到生成的纯色缓冲（不 panic）
fn load_tray_rgba() -> (Vec<u8>, u32, u32) {
    match image::load_from_memory(TRAY_ICON_PNG) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            (rgba.into_raw(), w, h)
        }
        Err(e) => {
            warn!("托盘图标解码失败，使用回退色块: {e}");
            solid_rgba(ACTIVE_COLOR, FALLBACK_ICON_SIZE)
        }
    }
}

/// 生成指定颜色的纯色 RGBA 缓冲（alpha=255）
fn solid_rgba(color: [u8; 3], size: u32) -> (Vec<u8>, u32, u32) {
    let mut v = Vec::with_capacity((size * size) as usize * 4);
    for _ in 0..(size * size) {
        v.extend_from_slice(&[color[0], color[1], color[2], 255]);
    }
    (v, size, size)
}

/// 由 RGBA 缓冲构造 [`Icon`]；失败时回退到纯色图标，再失败返回 None（不 panic）
fn make_icon(rgba: Vec<u8>, w: u32, h: u32) -> Option<Icon> {
    match Icon::from_rgba(rgba, w, h) {
        Ok(icon) => Some(icon),
        Err(e) => {
            debug!("托盘图标 RGBA 构造失败，回退纯色图标: {e}");
            let (buf, bw, bh) = solid_rgba(ACTIVE_COLOR, FALLBACK_ICON_SIZE);
            match Icon::from_rgba(buf, bw, bh) {
                Ok(icon) => Some(icon),
                Err(e) => {
                    debug!("托盘回退纯色图标构造失败: {e}");
                    None
                }
            }
        }
    }
}

/// 引擎状态 → 中文
fn engine_state_str(s: EngineState) -> &'static str {
    match s {
        EngineState::Running => "运行中",
        EngineState::Stopped => "已停止",
        EngineState::Dead => "已崩溃",
    }
}

/// 网络状态 → 中文
fn network_status_str(s: NetworkStatus) -> &'static str {
    match s {
        NetworkStatus::Online => "在线",
        NetworkStatus::CaptivePortal => "需认证",
        NetworkStatus::Offline => "离线",
        NetworkStatus::Unknown => "等待检测",
    }
}

/// 登录状态 → 中文
fn login_status_str(s: LoginStatus) -> &'static str {
    match s {
        LoginStatus::Idle => "空闲",
        LoginStatus::Running => "登录中",
        LoginStatus::Success => "成功",
        LoginStatus::Failed => "失败",
        LoginStatus::Cancelled => "已取消",
    }
}

/// Windows 专用：pump 当前线程消息队列中所有待处理的消息。
///
/// tray-icon 在 Windows 上为托盘创建隐藏窗口，窗口过程处理 `WM_USER_TRAYICON` 后
/// 通过 `TrayIconEvent::send` 分发到全局 handler；muda 菜单子类化也依赖窗口过程。
/// 必须由创建 `TrayIcon` 的同一线程周期性调用本函数（`PeekMessage` + `DispatchMessage`），
/// 否则窗口消息不被分发，托盘点击与菜单事件均不会触发。
#[cfg(windows)]
fn pump_windows_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// monitor_toggle_label：Running → 停止监测，Stopped/Dead → 启动监测
    #[test]
    fn test_monitor_toggle_label() {
        assert_eq!(monitor_toggle_label(EngineState::Running), "停止监测");
        assert_eq!(monitor_toggle_label(EngineState::Stopped), "启动监测");
        assert_eq!(monitor_toggle_label(EngineState::Dead), "启动监测");
    }

    /// engine_state_str：三个状态均有中文文案
    #[test]
    fn test_engine_state_str() {
        assert_eq!(engine_state_str(EngineState::Running), "运行中");
        assert_eq!(engine_state_str(EngineState::Stopped), "已停止");
        assert_eq!(engine_state_str(EngineState::Dead), "已崩溃");
    }

    /// network_status_str：四个网络状态均有中文文案
    #[test]
    fn test_network_status_str() {
        assert_eq!(network_status_str(NetworkStatus::Online), "在线");
        assert_eq!(network_status_str(NetworkStatus::CaptivePortal), "需认证");
        assert_eq!(network_status_str(NetworkStatus::Offline), "离线");
        assert_eq!(network_status_str(NetworkStatus::Unknown), "等待检测");
    }

    /// login_status_str：五个登录状态均有中文文案
    #[test]
    fn test_login_status_str() {
        assert_eq!(login_status_str(LoginStatus::Idle), "空闲");
        assert_eq!(login_status_str(LoginStatus::Running), "登录中");
        assert_eq!(login_status_str(LoginStatus::Success), "成功");
        assert_eq!(login_status_str(LoginStatus::Failed), "失败");
        assert_eq!(login_status_str(LoginStatus::Cancelled), "已取消");
    }

    /// menu_action_for：monitor_toggle 依引擎状态二选一；其余 id 固定映射；
    /// status_display 是禁用信息行，不映射任何动作
    #[test]
    fn test_menu_action_for() {
        let running = StatusSnapshot {
            engine_state: EngineState::Running,
            ..Default::default()
        };
        assert!(matches!(
            menu_action_for("monitor_toggle", &running),
            Some(TrayAction::StopMonitor)
        ));
        let stopped = StatusSnapshot {
            engine_state: EngineState::Stopped,
            ..Default::default()
        };
        assert!(matches!(
            menu_action_for("monitor_toggle", &stopped),
            Some(TrayAction::StartMonitor)
        ));
        let dead = StatusSnapshot {
            engine_state: EngineState::Dead,
            ..Default::default()
        };
        assert!(matches!(
            menu_action_for("monitor_toggle", &dead),
            Some(TrayAction::StartMonitor)
        ));
        assert!(matches!(
            menu_action_for("manual_login", &stopped),
            Some(TrayAction::ManualLogin)
        ));
        assert!(matches!(
            menu_action_for("open_web", &stopped),
            Some(TrayAction::OpenWeb)
        ));
        assert!(matches!(
            menu_action_for("copy_web_link", &stopped),
            Some(TrayAction::CopyWebLink)
        ));
        assert!(matches!(
            menu_action_for("quit", &stopped),
            Some(TrayAction::Quit)
        ));
        // 禁用信息行与未知 id 都不产生动作
        assert!(menu_action_for("status_display", &stopped).is_none());
        assert!(menu_action_for("nonexistent", &stopped).is_none());
        // 已移除的更新入口不得复活
        assert!(menu_action_for("check_update", &stopped).is_none());
    }

    /// status_item_enabled：仅引擎运行中为启用（借系统深色表达强调），
    /// 停止/崩溃均灰化
    #[test]
    fn test_status_item_enabled() {
        assert!(status_item_enabled(EngineState::Running));
        assert!(!status_item_enabled(EngineState::Stopped));
        assert!(!status_item_enabled(EngineState::Dead));
    }

    /// status_menu_label：始终含引擎与网络状态；登录态仅在非 Idle 时追加，
    /// 避免空闲时显示「· 空闲」这种无信息量的尾巴
    #[test]
    fn test_status_menu_label() {
        let online_idle = StatusSnapshot {
            engine_state: EngineState::Running,
            network_status: NetworkStatus::Online,
            login_status: LoginStatus::Idle,
            ..Default::default()
        };
        assert_eq!(status_menu_label(&online_idle), "状态：运行中 · 在线");

        let logging_in = StatusSnapshot {
            login_status: LoginStatus::Running,
            ..online_idle.clone()
        };
        assert_eq!(
            status_menu_label(&logging_in),
            "状态：运行中 · 在线 · 登录中"
        );

        let failed = StatusSnapshot {
            engine_state: EngineState::Stopped,
            network_status: NetworkStatus::CaptivePortal,
            login_status: LoginStatus::Failed,
            ..Default::default()
        };
        assert_eq!(
            status_menu_label(&failed),
            "状态：已停止 · 需认证 · 上次失败"
        );
    }

    /// solid_rgba：生成正确尺寸的纯色 RGBA 缓冲（alpha=255）
    #[test]
    fn test_solid_rgba() {
        let (buf, w, h) = solid_rgba([10, 20, 30], 4);
        assert_eq!((w, h), (4, 4));
        assert_eq!(buf.len(), 4 * 4 * 4);
        // 首像素 RGBA
        assert_eq!(&buf[..4], &[10, 20, 30, 255]);
    }
}
