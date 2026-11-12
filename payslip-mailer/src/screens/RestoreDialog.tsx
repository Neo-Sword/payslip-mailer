import { useEffect, useRef, useState } from "react";
// 导入改名：prop 也叫 `open`（布尔），同名会把插件函数遮住。
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { Button, Field, Modal, PasswordInput } from "../components/ui";
import { api, BackendError, type RestoreOutcome } from "../lib/api";
import s from "./BackupPanel.module.css";

/* ============================================================
   从备份恢复 · 对话框（TC-104 · ADR-0013 §4）
   **两处共用**：S14 的「数据备份」段，与 S02「忘记密码」弹窗里的那条出路。
   两处都要它，是因为「忘了口令」的人**进不去工作台**（标准 13）：
   出路不能藏在只有解锁后才看得到的地方。
   ============================================================ */

export function RestoreDialog({
  open,
  onClose,
  onDone,
}: {
  open: boolean;
  onClose: () => void;
  onDone: (outcome: RestoreOutcome) => void;
}) {
  const [path, setPath] = useState<string | null>(null);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pwRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setPath(null);
      setPassword("");
      setError(null);
    }
  }, [open]);

  const pick = async () => {
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "薪邮备份", extensions: ["zip"] }],
      });
      if (typeof picked === "string") {
        setPath(picked);
        setError(null);
        pwRef.current?.focus();
      }
    } catch (raw) {
      setError((raw as BackendError).message || "选不了文件，请重试。");
    }
  };

  const submit = async () => {
    if (!path) return;
    setBusy(true);
    setError(null);
    try {
      const outcome = await api.restoreBackup(path, password);
      onDone(outcome);
    } catch (raw) {
      setError((raw as BackendError).message || "恢复失败，请重试。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      title="从备份恢复"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" loading={busy} disabled={!path} onClick={submit}>
            开始恢复
          </Button>
        </>
      }
    >
      <div className={s.dialogBody}>
        <div className={s.pickRow}>
          <Button onClick={pick}>选择备份文件…</Button>
          <span className={s.pickedPath}>{path ?? "还没有选择文件"}</span>
        </div>

        <Field
          label="备份口令"
          htmlFor="restore-pw"
          required
          hint="导出这份备份时设置的那个口令。"
        >
          <PasswordInput
            id="restore-pw"
            ref={pwRef}
            value={password}
            autoComplete="off"
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>

        <p className={s.note}>
          恢复会<b>先自动备份当前的数据</b>，再替换它。口令不对或文件损坏时，
          现在的数据一个字都不会动。
        </p>

        {error && (
          <div className={s.error} role="alert">
            {error}
          </div>
        )}
      </div>
    </Modal>
  );
}
