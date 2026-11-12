import s from "./ReadOnlyBanner.module.css";

/* ============================================================
   只读态常驻横幅（spec 标准 16 · ADR-0012 第 5 条）
   用横幅不用 toast：只读是**整个会话的持续状态**，不是一次事件。
   可关掉的提示会在用户下一次尝试写入之前消失，于是那次写入的失败
   看起来像凭空冒出来的。

   文案是后端给的整句（`AdminState.readOnlyMessage`），与写被拒时
   返回的错误文案**同一个来源**（`infra::db::read_only_message`），
   前端一个字都不改、也不自己拼。
   ============================================================ */

export function ReadOnlyBanner({ message }: { message: string }) {
  return (
    <div className={s.banner} role="status">
      <span className={s.tag}>只读</span>
      <span className={s.text}>{message}</span>
    </div>
  );
}
