import { useCallback, useEffect, useState } from "react";
import { Button, Card, Field, Input, Select } from "../components/ui";
import { api, BackendError, type SmtpSettings, type TestResult } from "../lib/api";
import s from "./MailSettingsPanel.module.css";

/* ============================================================
   S14 · 邮件配置（TC-500 · ADR-0027）
   发信邮箱：host / port / 加密方式 / 账号 / 专用密码 / 发件人 / 回信地址 / 速率

   两条要紧的口径：
   1. **专用密码只进 Keychain**（ADR-0009）：保存后表里只有条目名，
      前端永远拿不到密码（红线 5）。
   2. **测试连接 = 发一封自检信给自己**（ch.7）—— 不是空连接探测：
      它把 AUTH 与 DATA 阶段都过了一遍。
   ============================================================ */

const EMPTY: SmtpSettings = {
  host: "smtp.feishu.cn",
  port: 465,
  security: "ssl",
  username: "",
  fromName: "",
  fromEmail: "",
  replyTo: "",
  ratePerMin: 30,
  credentialRef: "",
};

const RATE_OPTIONS = [20, 25, 30, 40, 60] as const;

export function MailSettingsPanel() {
  const [form, setForm] = useState<SmtpSettings>(EMPTY);
  const [password, setPassword] = useState("");
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<TestResult | null>(null);

  const refresh = useCallback(async () => {
    try {
      const s = await api.getSmtpSettings();
      if (s) setForm(s);
    } catch {
      /* 读不到保持空表单 */
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const set = (patch: Partial<SmtpSettings>) => setForm((f) => ({ ...f, ...patch }));

  const save = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const saved = await api.saveSmtpSettings(form, password);
      setForm(saved);
      setPassword("");
      setNotice(
        "发信配置已保存。专用密码已存进 macOS 钥匙串 —— 表里只有条目名，没有密码本体。",
      );
    } catch (raw) {
      setError((raw as BackendError).message || "保存失败，请检查各项后重试。");
    } finally {
      setBusy(false);
    }
  };

  const test = async () => {
    setTesting(true);
    setError(null);
    setNotice(null);
    setTestResult(null);
    try {
      const r = await api.testSmtpConnection(form, password);
      setTestResult(r);
    } catch (raw) {
      setError((raw as BackendError).message || "测试失败。");
    } finally {
      setTesting(false);
    }
  };

  return (
    <Card
      title="发信邮箱"
      meta={loaded && form.credentialRef ? "已配置" : "未配置"}
      actions={
        <>
          <Button loading={testing} disabled={busy} onClick={() => void test()}>
            测试连接
          </Button>
          <Button variant="primary" loading={busy} disabled={testing} onClick={() => void save()}>
            保存
          </Button>
        </>
      }
    >
      <div className={s.grid}>
        <Field label="SMTP 主机" required hint="飞书邮箱填 smtp.feishu.cn">
          <Input
            value={form.host}
            onChange={(e) => set({ host: e.target.value })}
            placeholder="smtp.feishu.cn"
          />
        </Field>
        <Field label="端口" required hint="SSL 用 465，STARTTLS 用 587">
          <Input
            type="number"
            value={form.port}
            min={1}
            max={65535}
            onChange={(e) => set({ port: Number(e.target.value) })}
          />
        </Field>
        <Field label="加密方式" required>
          <Select value={form.security} onChange={(e) => set({ security: e.target.value })}>
            <option value="ssl">SSL（implicit TLS，465）</option>
            <option value="starttls">STARTTLS（587）</option>
            <option value="none">无 TLS（仅内网）</option>
          </Select>
        </Field>
        <Field label="SMTP 用户名" required hint="飞书/腾讯 = 完整邮箱地址">
          <Input
            value={form.username}
            onChange={(e) => set({ username: e.target.value })}
            placeholder="hr@company.com"
            autoComplete="off"
          />
        </Field>
        <Field label="专用密码" required hint="飞书桌面客户端生成；只存进钥匙串，不进数据库">
          <Input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder={form.credentialRef ? "已保存（输入即覆盖）" : "专用密码"}
            autoComplete="new-password"
          />
        </Field>
        <Field label="发件人名称" hint="收件人看到的名字，例如「人力资源部」">
          <Input
            value={form.fromName}
            onChange={(e) => set({ fromName: e.target.value })}
            placeholder="人力资源部"
          />
        </Field>
        <Field label="发件地址" required hint="信封 From，一般与用户名相同">
          <Input
            value={form.fromEmail}
            onChange={(e) => set({ fromEmail: e.target.value })}
            placeholder="hr@company.com"
          />
        </Field>
        <Field label="回信地址" hint="模板变量 {{回信邮箱}}；留空则不设 Reply-To">
          <Input value={form.replyTo ?? ""} onChange={(e) => set({ replyTo: e.target.value })} />
        </Field>
        <Field label="发信速率" hint="每分钟几封；飞书保守值 20–30">
          <Select
            value={form.ratePerMin}
            onChange={(e) => set({ ratePerMin: Number(e.target.value) })}
          >
            {RATE_OPTIONS.map((r) => (
              <option key={r} value={r}>
                {r} 封 / 分钟
              </option>
            ))}
          </Select>
        </Field>
      </div>

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
      {testResult && (
        <div className={s.testBox} role="status">
          {testResult.kind === "ok" && (
            <span className={s.ok}>✓ 自检信已交给服务器 —— 收件箱里查收它，配置就通了。</span>
          )}
          {testResult.kind === "authFailed" && (
            <span className={s.bad}>
              认证失败（535）。请到飞书桌面客户端重新生成专用密码，然后在这里更新。
            </span>
          )}
          {testResult.kind === "failed" && (
            <span className={s.warn}>连接失败：{testResult.detail}</span>
          )}
        </div>
      )}

      <p className={s.note}>
        测试连接会给<b>发件地址自己</b>发一封自检信 —— 不是空连接探测，
        它把登录、发信、回执整个链路都走了一遍。
      </p>
    </Card>
  );
}
