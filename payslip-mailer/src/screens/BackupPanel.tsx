import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, Field, Modal, PasswordInput } from "../components/ui";
import { api, BackendError, type BackupFileInfo } from "../lib/api";
import { RestoreDialog } from "./RestoreDialog";
import { WipeDialog } from "./WipeDialog";
import s from "./BackupPanel.module.css";

/* ============================================================
   S14 · 设置 →「数据备份」段（TC-104 · ADR-0013）
   三件事：导出加密备份 / 看已有的备份 / 从备份恢复。

   两个关键口径（ADR-0013）：
   · **备份口令与管理员口令是两回事**：前者派生出备份密钥（备份的加密层），
     后者只用来登录本机。口令强度条与管理员口令同一条规则（≥10 位）。
   · **备份口令不设在线失败锁定**（ADR-0013 §5）：它保护的是离线文件，
     锁定给不了任何保护。界面上也不暗示有锁定。
   ============================================================ */

const MIN_PASSWORD_CHARS = 10;

/** 强度分（与管理员口令的强度条同一套算法） */
function passwordScore(password: string): number {
  if (password.length === 0) return 0;
  const kinds = [
    /[a-z]/,
    /[A-Z]/,
    /[0-9]/,
    /[^a-zA-Z0-9]/,
  ].filter((re) => re.test(password)).length;
  let score = 0;
  if (password.length >= MIN_PASSWORD_CHARS) score += 1;
  if (password.length >= 14) score += 1;
  if (kinds >= 3) score += 1;
  if (kinds === 4 && password.length >= 16) score += 1;
  return score;
}

const STRENGTH_TEXT = ["太短", "偏弱", "一般", "较强", "很强"];

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function formatTime(secs: number): string {
  const d = new Date(secs * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

export function BackupPanel({
  onRestored,
  onWiped,
}: {
  onRestored: () => void;
  /** 清空完：外层要重新读状态（回到 S01 首启向导） */
  onWiped: () => void;
}) {
  const [files, setFiles] = useState<BackupFileInfo[]>([]);
  const [exportOpen, setExportOpen] = useState(false);
  const [restoreOpen, setRestoreOpen] = useState(false);
  const [wipeOpen, setWipeOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [lastRestoredAt, setLastRestoredAt] = useState<number | null>(null);

  const refresh = useCallback(async () => {
    try {
      setFiles(await api.listBackups());
      const s = await api.getSettings();
      setLastRestoredAt(s.lastRestoredAt);
    } catch {
      /* 读不到就不显示，不打断设置页 */
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className={s.wrap}>
      <Card
        title="数据备份"
        actions={
          <>
            <Button size="sm" onClick={() => { setExportOpen(true); setError(null); }}>
              导出加密备份
            </Button>
            <Button size="sm" variant="primary" onClick={() => { setRestoreOpen(true); setError(null); }}>
              从备份恢复
            </Button>
          </>
        }
      >
        <p className={s.lead}>
          备份文件用<b>你设置的备份口令</b>加密（不是管理员口令），拷到 U 盘或网盘都能带走；
          在另一台电脑上用同一个口令就能恢复。文件名只有日期，不含任何工资数据。
        </p>

        {lastRestoredAt !== null && (
          <p className={s.restored}>上次从备份恢复：{formatTime(lastRestoredAt)}</p>
        )}

        {files.length === 0 ? (
          <div className={s.empty}>
            本机还没有备份。建议每周导出一次，放在与这台电脑不同的地方。
          </div>
        ) : (
          <div className={s.list}>
            {files.map((f) => (
              <div key={f.path} className={s.item}>
                <div className={s.itemMain}>
                  <span className={s.itemName}>{f.name}</span>
                  {f.kind === "preRestoreSafety" && (
                    <Chip tone="info">恢复前的保险</Chip>
                  )}
                </div>
                <div className={s.itemMeta}>
                  {formatTime(f.createdAt)} · {formatSize(f.size)} · 数据版本 v{f.schemaVersion}
                </div>
              </div>
            ))}
          </div>
        )}

        {notice && (
          <div className={s.notice} role="status">
            {notice}
          </div>
        )}
        {error && (
          <div className={s.error} role="alert">
            {error}
          </div>
        )}

        {/* 危险操作单独一块：红色 + 明确说不可撤销（设计文档 S14/S15） */}
        <div className={s.dangerZone}>
          <div className={s.dangerMain}>
            <div className={s.dangerTitle}>清空数据</div>
            <div className={s.dangerHint}>
              删除本机的全部工资数据、备份文件与库密钥。需要输入管理员密码并二次确认，<b>不可撤销</b>。
            </div>
          </div>
          <Button variant="danger" onClick={() => { setWipeOpen(true); setError(null); }}>
            清空数据…
          </Button>
        </div>
      </Card>

      <ExportDialog
        open={exportOpen}
        onClose={() => setExportOpen(false)}
        onDone={async (name) => {
          setExportOpen(false);
          setNotice(`已导出 ${name}`);
          await refresh();
        }}
      />

      <RestoreDialog
        open={restoreOpen}
        onClose={() => setRestoreOpen(false)}
        onDone={async (outcome) => {
          setRestoreOpen(false);
          setNotice(
            outcome.adminEmail
              ? `恢复完成。请用备份里那时的管理员口令（${outcome.adminEmail}）解锁。`
              : "恢复完成。",
          );
          await refresh();
          onRestored();
        }}
      />

      <WipeDialog
        open={wipeOpen}
        onClose={() => setWipeOpen(false)}
        onDone={() => {
          setWipeOpen(false);
          onWiped();
        }}
      />
    </div>
  );
}

/* ---------------- 导出 ---------------- */

function ExportDialog({
  open,
  onClose,
  onDone,
}: {
  open: boolean;
  onClose: () => void;
  onDone: (name: string) => void;
}) {
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const score = passwordScore(password);
  const longEnough = password.length >= MIN_PASSWORD_CHARS;

  useEffect(() => {
    if (open) {
      setPassword("");
      setError(null);
    }
  }, [open]);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const info = await api.exportBackup(password);
      onDone(info.name);
    } catch (raw) {
      setError((raw as BackendError).message || "导出失败，请重试。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      title="导出加密备份"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" loading={busy} disabled={!longEnough} onClick={submit}>
            导出
          </Button>
        </>
      }
    >
      <div className={s.dialogBody}>
        <Field
          label="备份口令"
          htmlFor="backup-pw"
          required
          hint="至少 10 位。恢复时要用同一个口令——忘了就没有别的办法找回。"
        >
          <PasswordInput
            id="backup-pw"
            value={password}
            autoComplete="new-password"
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>

        <div className={s.strength} aria-label="备份口令强度">
          {[0, 1, 2, 3].map((i) => (
            <i key={i} className={i < score ? s.strengthOn : undefined} />
          ))}
          <span className={s.strengthText}>
            {password.length > 0 ? STRENGTH_TEXT[score] : "口令强度"}
          </span>
        </div>

        <p className={s.note}>
          这个口令<b>不是</b>管理员口令，只用来加密备份文件。它不会被存到任何地方，
          也不会出现在日志里。
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
