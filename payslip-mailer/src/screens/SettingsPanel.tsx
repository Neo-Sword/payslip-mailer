import { useEffect, useRef, useState } from "react";
import { Button, Chip, Field, Modal, PasswordInput, Select } from "../components/ui";
import { MailSettingsPanel } from "./MailSettingsPanel";
import { api, BackendError, type SettingsState } from "../lib/api";
import { BackupPanel } from "./BackupPanel";
import s from "./SettingsPanel.module.css";

/* ============================================================
   S14 · 设置（设计文档 ch.12 S14）
   本卡只做「安全与锁定」这一个 tab（spec 范围 11）：
   - 空闲自动锁定 5/10/30 下拉（改完当前会话立刻生效，不重启）
   - 修改管理员密码
   - 数据库「SQLCipher 已加密」chip
   - 数据目录路径（与 app_info 返回的 data_dir 逐字相同，spec 标准 18）

   邮件配置 / 数据备份 / 关于三个 tab 本卡不做，点了就说清归哪张卡。
   ============================================================ */

const IDLE_OPTIONS = [5, 10, 30] as const;

const TABS = [
  { id: "mail", label: "邮件配置", owner: null },
  { id: "security", label: "安全与锁定", owner: null },
  { id: "backup", label: "数据备份", owner: null },
  { id: "about", label: "关于", owner: null },
] as const;

export function SettingsPanel({
  dataDir,
  version,
  privacyNote,
  onLockNow,
  onRestored,
  onWiped,
}: {
  dataDir: string | null;
  version: string;
  privacyNote: string;
  onLockNow: () => void;
  /** 恢复完数据后要刷新外层（工作台的成员/批次可能都变了） */
  onRestored: () => void;
  /** 清空完：外层要重新读状态（会回到 S01 首启向导） */
  onWiped: () => void;
}) {
  const [tab, setTab] = useState<"mail" | "security" | "backup" | "about">("security");
  const [settings, setSettings] = useState<SettingsState | null>(null);
  const [minutes, setMinutes] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [pwOpen, setPwOpen] = useState(false);
  const [pwBusy, setPwBusy] = useState(false);
  const [pwError, setPwError] = useState<string | null>(null);
  const [pwDone, setPwDone] = useState(false);
  const oldRef = useRef<HTMLInputElement>(null);
  const newRef = useRef<HTMLInputElement>(null);
  const new2Ref = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let alive = true;
    api
      .getSettings()
      .then((s) => {
        if (!alive) return;
        setSettings(s);
        setMinutes(s.idleLockMinutes);
      })
      .catch(() => {
        /* 读不到就保持空态，不打断设置页 */
      });
    return () => {
      alive = false;
    };
  }, []);

  const changeIdle = async (next: number) => {
    setMinutes(next);
    setSaving(true);
    setError(null);
    setNote(null);
    try {
      await api.setIdleLockMinutes(next);
      setNote(`空闲自动锁定已改为 ${next} 分钟，立即生效（不用重启）。`);
    } catch (raw) {
      const err = raw as BackendError;
      setError(err.message || "设置没有保存成功。");
      setMinutes(settings?.idleLockMinutes ?? null);
    } finally {
      setSaving(false);
    }
  };

  const changePassword = async () => {
    const oldPassword = oldRef.current?.value ?? "";
    const newPassword = newRef.current?.value ?? "";
    const again = new2Ref.current?.value ?? "";
    if (newPassword !== again) {
      setPwError("两次输入的新密码不一致");
      return;
    }
    if (newPassword.length < 10) {
      setPwError("新密码至少 10 位")
      return;
    }
    setPwBusy(true);
    setPwError(null);
    try {
      await api.changePassword(oldPassword, newPassword);
      setPwDone(true);
      setPwOpen(false);
      setNote("管理员密码已修改。");
    } catch (raw) {
      const err = raw as BackendError;
      setPwError(err.message || "密码没有修改成功。");
    } finally {
      setPwBusy(false);
    }
  };

  return (
    <div className={s.wrap}>
      <div className={s.tabs} role="tablist" aria-label="设置分类">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`tab-${t.id}`}
            aria-controls={`panel-${t.id}`}
            aria-selected={tab === t.id}
            className={[s.tab, tab === t.id ? s.tabActive : undefined].filter(Boolean).join(" ")}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      {tab === "security" && (
        <div id="panel-security" role="tabpanel" aria-labelledby="tab-security" className={s.panel}>
          <section className={s.block}>
            <h3 className={s.blockTitle}>安全</h3>
            <div className={s.row}>
              <div className={s.rowMain}>
                <div className={s.rowLabel}>空闲自动锁定</div>
                <div className={s.rowHint}>放着不动到点自动上锁，密钥从内存里释放。</div>
              </div>
              <div className={s.rowSide}>
                <Select
                  aria-label="空闲自动锁定分钟数"
                  value={minutes ?? undefined}
                  disabled={minutes === null || saving}
                  onChange={(e) => changeIdle(Number(e.target.value))}
                >
                  {IDLE_OPTIONS.map((m) => (
                    <option key={m} value={m}>
                      {m} 分钟
                    </option>
                  ))}
                </Select>
              </div>
            </div>

            <div className={s.row}>
              <div className={s.rowMain}>
                <div className={s.rowLabel}>修改管理员密码</div>
                <div className={s.rowHint}>需要输入当前密码；修改后下次解锁用新密码。</div>
              </div>
              <div className={s.rowSide}>
                <Button onClick={() => { setPwOpen(true); setPwDone(false); setPwError(null); }}>
                  修改…
                </Button>
              </div>
            </div>
          </section>

          <section className={s.block}>
            <h3 className={s.blockTitle}>数据库</h3>
            <div className={s.row}>
              <div className={s.rowMain}>
                <div className={s.rowLabel}>存储方式</div>
                <div className={s.rowHint}>
                  数据目录 <code className={s.path}>{dataDir ?? "（读取中）"}</code>
                </div>
              </div>
              <div className={s.rowSide}>
                <Chip tone="ok">SQLCipher 已加密</Chip>
              </div>
            </div>
          </section>

          {note && (
            <div className={s.note} role="status">
              {note}
            </div>
          )}
          {error && (
            <div className={s.error} role="alert">
              {error}
            </div>
          )}

          <div className={s.lockNow}>
            <Button variant="danger" onClick={onLockNow}>
              ⌘L 立即锁定
            </Button>
          </div>
        </div>
      )}

      {tab === "about" && (
        <div id="panel-about" role="tabpanel" aria-labelledby="tab-about" className={s.panel}>
          <section className={s.block}>
            <h3 className={s.blockTitle}>关于</h3>
            <div className={s.row}>
              <div className={s.rowMain}>
                <div className={s.rowLabel}>薪邮 Payslip Mailer</div>
                <div className={s.rowHint}>{privacyNote}</div>
              </div>
              <div className={s.rowSide}>
                <Chip>v{version}</Chip>
              </div>
            </div>
            <div className={s.row}>
              <div className={s.rowMain}>
                <div className={s.rowLabel}>本机数据目录</div>
                <div className={s.rowHint}>
                  备份与恢复都按这个目录找文件。
                </div>
              </div>
              <div className={s.rowSide}>
                <code className={s.path}>{dataDir ?? "（读取中）"}</code>
              </div>
            </div>
          </section>
        </div>
      )}

      {tab === "backup" && (
        <div
          id="panel-backup"
          role="tabpanel"
          aria-labelledby="tab-backup"
          className={s.panel}
        >
          <BackupPanel onRestored={onRestored} onWiped={onWiped} />
        </div>
      )}

      {tab === "mail" && (
        <div
          id="panel-mail"
          role="tabpanel"
          aria-labelledby="tab-mail"
          className={s.panel}
        >
          <MailSettingsPanel />
        </div>
      )}

      <Modal
        open={pwOpen}
        title="修改管理员密码"
        onClose={() => setPwOpen(false)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setPwOpen(false)}>
              取消
            </Button>
            <Button variant="primary" loading={pwBusy} onClick={changePassword}>
              保存
            </Button>
          </>
        }
      >
        <div className={s.pwBody}>
          <Field label="当前密码" htmlFor="pw-old" required>
            <PasswordInput id="pw-old" ref={oldRef} autoComplete="current-password" />
          </Field>
          <Field label="新密码（至少 10 位）" htmlFor="pw-new" required>
            <PasswordInput id="pw-new" ref={newRef} autoComplete="new-password" />
          </Field>
          <Field label="确认新密码" htmlFor="pw-new2" required>
            <PasswordInput id="pw-new2" ref={new2Ref} autoComplete="new-password" />
          </Field>
          {pwError && (
            <div className={s.error} role="alert">
              {pwError}
            </div>
          )}
          {pwDone && (
            <div className={s.note} role="status">
              密码已修改。
            </div>
          )}
        </div>
      </Modal>
    </div>
  );
}
