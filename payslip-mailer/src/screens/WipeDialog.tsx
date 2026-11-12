import { useEffect, useState } from "react";
import { Button, Chip, Modal } from "../components/ui";
import { api, BackendError, type WipePreview } from "../lib/api";
import s from "./BackupPanel.module.css";

/* ============================================================
   清空数据 · 危险确认框（TC-105 · ADR-0014）
   文案逐字对齐设计文档 S15 的原型：
     「确认清空全部本地数据？」
     「将永久删除……操作不可撤销。」
     「请输入管理员密码以继续：」

   三条硬要求：
   · **不输口令就不能确认**（设计文档：二次确认 + 输入管理员密码）
   · 按钮是危险色（S15 交互要点：「危险操作红色 + 密码确认」）
   · **逐项列出**会被删掉什么 —— 特别是备份文件（那是不可逆的那部分）
   ============================================================ */

export function WipeDialog({
  open,
  onClose,
  onDone,
}: {
  open: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const [preview, setPreview] = useState<WipePreview | null>(null);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setPassword("");
    setError(null);
    setPreview(null);
    void api
      .wipePreview()
      .then(setPreview)
      .catch(() => setPreview(null));
  }, [open]);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.wipeData(password);
      onDone();
    } catch (raw) {
      setError((raw as BackendError).message || "清空没有完成，请重试。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      title="确认清空全部本地数据？"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button
            variant="danger"
            loading={busy}
            disabled={password.length === 0}
            onClick={submit}
          >
            确认清空
          </Button>
        </>
      }
    >
      <div className={s.dialogBody}>
        <p className={s.note}>
          将永久删除本机的全部工资数据，操作<b>不可撤销</b>。
        </p>

        <div className={s.wipeList}>
          <div className={s.wipeRow}>
            <Chip tone="danger">会删</Chip>
            <span>数据库（成员名单、工资数据、批次记录）</span>
          </div>
          <div className={s.wipeRow}>
            <Chip tone="danger">会删</Chip>
            <span>钥匙串里的库密钥（删了它，密文就再也解不开）</span>
          </div>
          <div className={s.wipeRow}>
            <Chip tone="danger">会删</Chip>
            <span>
              本机的备份文件
              {preview && preview.backupNames.length > 0
                ? `（${preview.backupNames.length} 个：${preview.backupNames.join("、")}）`
                : ""}
            </span>
          </div>
          <div className={s.wipeRow}>
            <Chip tone="info">保留</Chip>
            <span>日志（已脱敏，不含工资数值与邮箱正文）</span>
          </div>
        </div>

        <p className={s.warnText}>
          <b>备份文件也会一起删除。</b>
          要是想留一份，请<b>先把它移到这台电脑之外</b>，再回来执行清空。
        </p>

        <label className={s.wipeLabel} htmlFor="wipe-pw">
          请输入管理员密码以继续：
        </label>
        <input
          id="wipe-pw"
          type="password"
          className={s.wipeInput}
          value={password}
          autoComplete="off"
          onChange={(e) => setPassword(e.target.value)}
          placeholder="管理员密码"
        />

        {error && (
          <div className={s.error} role="alert">
            {error}
          </div>
        )}
      </div>
    </Modal>
  );
}
