'use strict';
'require view';
'require form';
'require uci';
'require rpc';
'require fs';
'require ui';
'require poll';

// 服务动作只使用固定参数，ACL 不开放任意命令或数据文件读取权限。
var callService = rpc.declare({
	object: 'service',
	method: 'list',
	params: ['name'],
	expect: { '': {} }
});

function consoleURL(host, port, hostname) {
	if (!host || /^127\./.test(host) || host === '::1' || host === '0:0:0:0:0:0:0:1')
		return null;
	if (!/^\d+$/.test(String(port)) || +port < 1 || +port > 65535)
		return null;
	host = (host === '0.0.0.0' || host === '::') ? hostname : host;
	// 链接只允许 IP 地址或当前页面主机，避免存量配置生成可执行 URL。
	if (!host || !/^[a-zA-Z0-9.:[\]-]+$/.test(host))
		return null;
	if (host.indexOf(':') !== -1 && host.charAt(0) !== '[')
		host = '[' + host + ']';
	return 'http://' + host + ':' + port + '/';
}

return view.extend({
	load: function() {
		return Promise.all([uci.load('campus-auth'), this.readStatus()]);
	},

	readStatus: function() {
		return Promise.all([
			callService('campus-auth'),
			fs.exec('/etc/init.d/campus-auth', ['enabled'])
		]).then(function(data) {
			if (data[1].code !== 0 && data[1].code !== 1)
				throw new Error(data[1].stderr || _('无法读取开机启动状态。'));
			var service = data[0]['campus-auth'] || {};
			var instances = service.instances || {};
			return {
				running: Object.keys(instances).some(function(key) { return instances[key].running === true; }),
				autostart: data[1].code === 0
			};
		});
	},

	refreshStatus: function() {
		return this.readStatus().then(L.bind(function(state) {
			this.statusNode.textContent = state.running ? _('运行中') : _('已停止');
			this.bootNode.textContent = state.autostart ? _('开机启动已开启') : _('开机启动已关闭');
		}, this)).catch(L.bind(function(error) {
			this.statusNode.textContent = _('无法读取状态：') + error.message;
		}, this));
	},

	runAction: function(action, event) {
		var button = event.currentTarget;
		button.disabled = true;
		return fs.exec('/etc/init.d/campus-auth', [action]).then(function(result) {
			if (result.code !== 0)
				throw new Error(result.stderr || _('服务命令执行失败，请查看系统日志。'));
		}).catch(function(error) {
			ui.addNotification(null, E('p', {}, error.message), 'error');
		}).then(L.bind(this.refreshStatus, this)).finally(function() {
			button.disabled = false;
		});
	},

	render: function(data) {
		var m = new form.Map('campus-auth', _('认证喵'),
			_('校园网 HTTP / 脚本自动认证。账号、登录任务与网络监测在认证控制台配置。保存并应用后，点击重启服务使监听设置生效。'));
		var s = m.section(form.NamedSection, 'main', 'campus-auth', _('服务设置'));
		s.addremove = false;
		var o = s.option(form.Flag, 'enabled', _('允许运行服务'),
			_('关闭后服务不会启动；开机启动由下方独立按钮控制。'));
		o.default = '1';
		o.rmempty = false;
		o = s.option(form.Value, 'host', _('监听地址'),
			_('默认 127.0.0.1，只能通过 SSH 隧道访问。直接从局域网访问时填写路由器 LAN IP，并仅在可信 LAN 使用。'));
		o.datatype = 'ipaddr';
		o.default = '127.0.0.1';
		o.rmempty = false;
		o = s.option(form.Value, 'port', _('控制台端口'));
		o.datatype = 'port';
		o.default = '50721';
		o.rmempty = false;
		o.validate = function(section, value) {
			return /^\d+$/.test(value) && +value >= 1 && +value <= 65535 ? true : _('端口必须在 1 至 65535 之间。');
		};
		o = s.option(form.Value, 'data_dir', _('数据目录'),
			_('使用专用持久化目录，不可填写 /etc、/root 等系统目录。更改前手动迁移原有数据，自定义目录需另行加入固件升级备份。'));
		o.default = '/etc/campus-auth';
		o.rmempty = false;
		o.validate = function(section, value) {
			return value && value.charAt(0) === '/' && value !== '/' ? true : _('请输入非根目录的绝对路径。');
		};

		this.statusNode = E('strong', {}, data[1].running ? _('运行中') : _('已停止'));
		this.bootNode = E('span', {}, data[1].autostart ? _('开机启动已开启') : _('开机启动已关闭'));
		var buttons = [['start', _('启动')], ['stop', _('停止')], ['restart', _('重启')],
			['enable', _('开启开机启动')], ['disable', _('关闭开机启动')]].map(L.bind(function(item) {
			return E('button', {
				'class': 'cbi-button cbi-button-action',
				'click': L.bind(this.runAction, this, item[0]),
				'disabled': !L.hasViewPermission()
			}, item[1]);
		}, this));
		var url = consoleURL(uci.get('campus-auth', 'main', 'host') || '127.0.0.1',
			uci.get('campus-auth', 'main', 'port') || '50721', window.location.hostname);
		var consoleNode = url
			? E('a', { 'class': 'cbi-button cbi-button-action', 'href': url, 'target': '_blank', 'rel': 'noopener noreferrer' }, _('打开认证控制台'))
			: E('p', {}, _('回环监听请先在电脑执行 ssh -L 50721:127.0.0.1:50721 root@路由器地址，再访问 http://127.0.0.1:50721；端口以实际配置为准。'));
		poll.add(L.bind(this.refreshStatus, this), 5);
		return m.render().then(L.bind(function(mapNode) {
			return E('div', {}, [
				E('h2', {}, _('认证喵服务')),
				E('p', {}, [this.statusNode, ' · ', this.bootNode]),
				E('div', { 'class': 'cbi-section' }, buttons),
				consoleNode,
				E('p', {}, _('认证控制台独立监听，不会继承 LuCI 登录会话。日志请在“状态 → 系统日志”查看 campus-auth。')),
				mapNode
			]);
		}, this));
	}
});
