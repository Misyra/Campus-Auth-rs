/** 全局 body 滚动锁（FE2-2/FE2-3）：模块级计数收口所有弹窗/遮罩的锁定需求 */

/** 打开中的滚动锁计数；0 表示未锁定。模块级共享，嵌套弹窗共用同一计数 */
let lockCount = 0;

/** 之前是否由本锁写入过 overflow（body 可能被第三方写成了非空值） */
let lockApplied = false;

/** 获取一个滚动锁：计数 +1，首次加锁时隐藏 body 滚动 */
export function lockBodyScroll(): void {
  lockCount += 1;
  if (lockCount === 1) {
    document.body.style.overflow = "hidden";
    lockApplied = true;
  }
}

/** 释放一个滚动锁：计数 -1，归零时恢复 body 滚动 */
export function unlockBodyScroll(): void {
  lockCount = Math.max(0, lockCount - 1);
  if (lockCount === 0 && lockApplied) {
    document.body.style.overflow = "";
    lockApplied = false;
  }
}

/** 当前是否处于锁定状态（调试/测试用） */
export function isBodyScrollLocked(): boolean {
  return lockCount > 0;
}
