import { useEffect, useRef, useState, type FormEvent } from "react";
import { Button, Field, Modal, PasswordInput, Switch } from "../components/ui";
import { api, BackendError } from "../lib/api";
import type { AdminState } from "../lib/api";
import s from "./LockScreen.module.css";

/* ============================================================
   S02 · 登录 / 解锁（设计文档 ch.12 S02 逐字对齐）
   回车即提交；失败次数与倒计时可见；「忘记密码」只给诚实引导。

   口令同样不进 React state：输入框非受控，提交时从 DOM 读值。
   ============================================================ */

export function LockScreen({
  status,
  version,
  onUnlocked,
  onRestoreRequest,
}: {
  status: AdminState;
  version: string;
  onUnlocked: () => void;
  /** 「忘记密码」→ 从备份恢复（TC-104 · 标准 13）：
   *  忘了口令的人**进不去工作台**，所以出路不能只挂在设置页里。 */
  onRestoreRequest: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [forgot, setForgot] = useState(false);
  const [remember, setRemember] = useState(true);

  // 倒计时：后端给的是「还剩几秒」，本地每秒减一，减到 0 就重新问一次后端。
  const [secondsLeft, setSecondsLeft] = useState(status.secondsLeft);
  const [locked, setLocked] = useState(status.locked);

  const pwRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setSecondsLeft(status.secondsLeft);
    setLocked(status.locked);
  }, [status.secondsLeft, status.locked]);

  useEffect(() => {
    if (!locked || secondsLeft <= 0) return;
    const t = setInterval(() => setSecondsLeft((v) => Math.max(0, v - 1)), 1000);
    return () => clearInterval(t);
  }, [locked, secondsLeft]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const password = pwRef.current?.value ?? "";
    setBusy(true);
    setError(null);
    try {
      await api.unlock(password, remember);
      onUnlocked();
    } catch (raw) {
      const err = raw as BackendError;
      setError(err.message || "解锁失败，请重试。");
      // 口令错了：清空输入框，让下一次重新输
      if (pwRef.current) pwRef.current.value = "";
      if (err.code === "LOCKED") setLocked(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={s.wrap}>
      <div className={s.badgeRow}>
        <span className={s.badge}>已锁定</span>
      </div>
      <div className={s.logo} aria-hidden>
        薪
      </div>
      <h1 className={s.title}>欢迎回来</h1>
      {status.email && <p className={s.email}>{status.email}</p>}

      <form onSubmit={submit} className={s.form}>
        <Field label="登录密码" htmlFor="s02-pw">
          <PasswordInput
            id="s02-pw"
            ref={pwRef}
            autoComplete="current-password"
            autoFocus
            invalid={error !== null}
          />
        </Field>

        {error && (
          <div className={s.error} role="alert">
            {error}
          </div>
        )}

        {!locked && status.remainingAttempts < 5 && (
          <div className={s.remaining} role="status">
            还可尝试 {status.remainingAttempts} 次
          </div>
        )}

        {locked && (
          <div className={s.countdown} role="status">
            已锁定，请等待 {secondsLeft} 秒后重试
          </div>
        )}

        <div className={s.rememberRow}>
          <Switch checked={remember} onChange={setRemember} label="记住本机 10 分钟" />
          <span className={s.rememberText}>记住本机 10 分钟</span>
        </div>

        <Button type="submit" variant="primary" block loading={busy}>
          解锁并进入工作台
        </Button>
      </form>

      <button type="button" className={s.forgot} onClick={() => setForgot(true)}>
        忘记密码？
      </button>

      <div className={s.warn}>
        密码连续错误 5 次将锁定 60 秒。本机数据已加密，无法通过重装应用绕过。
      </div>

      <p className={s.foot}>薪邮 v{version} · 数据仅存本机 · 无遥测</p>

      <Modal open={forgot} title="忘记密码" onClose={() => setForgot(false)}>
        <div className={s.forgotBody}>
          <p className={s.forgotLead}>
            管理员口令只以 Argon2id 哈希保存在本机，<b>无法明文找回</b>，也没有服务器可以重置。
          </p>
          <p className={s.forgotText}>唯一的出路是从加密备份恢复：</p>
          <ul className={s.forgotList}>
            <li>用备份文件恢复——点下面的「从备份恢复…」，选一份备份并输入它的备份口令。</li>
            <li>如果还没有备份，就只能重新初始化这台机器——旧数据文件仍在原处，重新初始化不会删它。</li>
          </ul>
          <p className={s.forgotText}>钥匙串里那把库密钥丢失时（应用升级会重置授权），也请走「重试」而不是重置密码。</p>
        </div>
        <div className={s.forgotFoot}>
          <Button variant="ghost" onClick={() => { setForgot(false); onRestoreRequest(); }}>
            从备份恢复…
          </Button>
          <Button variant="primary" onClick={() => setForgot(false)}>
            我知道了
          </Button>
        </div>
      </Modal>
    </div>
  );
}
