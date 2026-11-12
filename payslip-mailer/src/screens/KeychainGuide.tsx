import { useState } from "react";
import { Button } from "../components/ui";
import { api, BackendError } from "../lib/api";
import s from "./KeychainGuide.module.css";

/* ============================================================
   KEYCHAIN_MISSING 引导（spec 标准 15 · ADR-0012 第 9 条）
   两条分支都必须有真能点的动作：
   - 重试：覆盖「应用升级后钥匙串授权被重置」这个正常场景
     （用户在「密码与密钥」里重新授权或系统弹窗点了「允许」之后就能接上）。
     重试走的是同一次 admin_status，**不新建空库**——库层的守卫保证
     （钥匙串没密钥且库文件还在 → 原样报 KeychainMissing）。
   - 重新初始化：走 S01 重新建档。必须明说旧的 data.db 仍在原目录、
     只是当前打不开，且重新初始化不会删它（删与恢复归 TC-104）。
   ============================================================ */

export function KeychainGuide({
  dataDir,
  onRetrySucceeded,
  onReinitialize,
}: {
  dataDir: string | null;
  onRetrySucceeded: () => void;
  onReinitialize: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const retry = async () => {
    setBusy(true);
    setMessage(null);
    try {
      await api.adminStatus();
      // 能读到状态就说明钥匙串通了
      onRetrySucceeded();
    } catch (raw) {
      const err = raw as BackendError;
      setMessage(
        err.code === "KEYCHAIN_MISSING"
          ? "钥匙串里仍然没有那把库密钥。请在「密码与密钥」里恢复对本应用的授权后，再点一次「重试」。"
          : err.message || "重试没有成功，请稍后再试。",
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={s.wrap}>
      <div className={s.logo} aria-hidden>
        薪
      </div>
      <h1 className={s.title}>打不开本机的加密数据</h1>
      <p className={s.lead}>
        钥匙串里找不到这把加密库的密钥，所以本机数据暂时读不出来。
        你的数据文件还在，没有被删除或覆盖。
      </p>

      <div className={s.card}>
        <div className={s.cardTitle}>可能的原因</div>
        <ul className={s.list}>
          <li>应用升级后，macOS 重置了钥匙串条目的授权（未签名分发的正常现象）。</li>
          <li>刚才弹出的授权窗被点了「拒绝」。</li>
          <li>钥匙串条目被别的清理工具删掉了。</li>
        </ul>
        {dataDir && (
          <p className={s.path}>
            旧数据文件仍在：<code>{dataDir}/data.db</code>
            <br />
            下面的两个动作都不会删除它（重新初始化只是把它改名移走）。
          </p>
        )}
      </div>

      {message && (
        <div className={s.message} role="alert">
          {message}
        </div>
      )}

      <div className={s.actions}>
        <Button variant="primary" loading={busy} onClick={retry}>
          重试
        </Button>
        <Button onClick={onReinitialize}>重新初始化</Button>
      </div>

      <p className={s.foot}>
        「重新初始化」会把旧库改名移到一边（<b>不删除</b>），然后重新走一遍首启向导。
        想先试试能不能接上，请点「重试」。
      </p>
    </div>
  );
}
