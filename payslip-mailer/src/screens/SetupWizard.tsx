import { useRef, useState } from "react";
import { Button, Field, Input, PasswordInput, Steps } from "../components/ui";
import { api, BackendError, isPreview } from "../lib/api";
import s from "./SetupWizard.module.css";

/* ============================================================
   S01 · 首次启动向导（设计文档 ch.12 S01 逐字对齐）
   三步：1 创建管理员 / 2 配置发件邮箱 / 3 完成引导

   口令的处理是这一屏的关键（spec 标准 20）：
   口令**不进 React state**，只存在输入框的 DOM 里。表单用非受控 input + ref，
   state 里只留「够不够长」「两次一不一样」「强度几分」「有几个字」这类
   布尔与数字。提交时再从 DOM 读一次值传给后端。
   这样 DevTools 的状态树里搜不到口令，渲染日志里也不会出现它。
   ============================================================ */

/** 与 Rust 侧 `domain::admin::check_email_format` 同一套规则（前端实时、后端兜底） */
export function isEmailFormatOk(email: string): boolean {
  if (email.length === 0) return false;
  if (/\s/.test(email)) return false;
  const parts = email.split("@");
  if (parts.length !== 2) return false;
  const [local, domain] = parts;
  return local.length > 0 && domain.length > 0;
}

/** 密码强度 0–4：只看长度与字符种类，不看具体内容（强度条不该泄露口令特征） */
export function passwordScore(password: string): number {
  const len = password.length;
  if (len === 0) return 0;
  let kinds = 0;
  if (/[a-z]/.test(password)) kinds += 1;
  if (/[A-Z]/.test(password)) kinds += 1;
  if (/[0-9]/.test(password)) kinds += 1;
  if (/[^a-zA-Z0-9]/.test(password)) kinds += 1;
  let score = 0;
  if (len >= 10) score += 1;
  if (len >= 14) score += 1;
  if (kinds >= 3) score += 1;
  if (kinds === 4 && len >= 16) score += 1;
  return score;
}

export const MIN_PASSWORD_CHARS = 10;

const STRENGTH_TEXT = ["太短", "偏弱", "一般", "较强", "很强"];

export function SetupWizard({
  version,
  dataDir,
  /** 「重新初始化」模式：旧的 data.db 还在原处，要先说清楚 */
  reinitialize,
  onDone,
}: {
  version: string;
  dataDir: string | null;
  reinitialize?: boolean;
  onDone: () => void;
}) {
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  // 下面五个 state 只存「判定结果」与「长度计数」，一个秘密都不存
  const [emailOk, setEmailOk] = useState(false);
  const [emailLen, setEmailLen] = useState(0);
  const [score, setScore] = useState(0);
  const [pwLen, setPwLen] = useState(0);
  const [same, setSame] = useState(true);
  const [pw2Len, setPw2Len] = useState(0);

  const emailRef = useRef<HTMLInputElement>(null);
  const pwRef = useRef<HTMLInputElement>(null);
  const pw2Ref = useRef<HTMLInputElement>(null);
  const orgRef = useRef<HTMLInputElement>(null);

  /** 重新算一遍判定：从 DOM 读值，只把结论与长度写进 state */
  const revalidate = () => {
    const email = emailRef.current?.value ?? "";
    const pw = pwRef.current?.value ?? "";
    const pw2 = pw2Ref.current?.value ?? "";
    setEmailOk(isEmailFormatOk(email));
    setEmailLen(email.length);
    setScore(passwordScore(pw));
    setPwLen(pw.length);
    setSame(pw === pw2);
    setPw2Len(pw2.length);
  };

  const passwordOk = pwLen >= MIN_PASSWORD_CHARS;
  const canNext = emailOk && passwordOk && same;

  const finish = async () => {
    const email = (emailRef.current?.value ?? "").trim();
    const password = pwRef.current?.value ?? "";
    const orgName = (orgRef.current?.value ?? "").trim();
    setBusy(true);
    setFormError(null);
    try {
      await api.createAdmin(email, password, orgName);
      onDone();
    } catch (raw) {
      const err = raw as BackendError;
      setFormError(err.message || "初始化未完成，请重试。");
    } finally {
      setBusy(false);
    }
  };

  const stepItems = [
    { label: "创建管理员", state: step === 0 ? "active" : "done" },
    { label: "配置发件邮箱", state: step === 1 ? "active" : step === 2 ? "done" : "pending" },
    { label: "完成引导", state: step === 2 ? "active" : "pending" },
  ] as { label: string; state: "pending" | "active" | "done" }[];

  return (
    <div className={s.wrap}>
      <div className={s.logo} aria-hidden>
        薪
      </div>
      <h1 className={s.title}>欢迎使用薪邮</h1>
      <p className={s.sub}>只需 3 步，即可开始安全地发送工资条</p>

      <div className={s.stepsBox}>
        <Steps items={stepItems} />
      </div>

      {reinitialize && (
        <div className={s.notice} role="alert">
          <b>重新初始化前请先读这段。</b>
          <br />
          旧的 <code>data.db</code> 仍在原目录（{dataDir ?? "本机数据目录"}/data.db），
          重新初始化<b>不会删除它</b>。当前打不开它，是因为钥匙串里那把库密钥找不到了。
          请先在「密码与密钥」里恢复对钥匙串的授权后点「重试」；若确定要重新开始，
          请把旧文件改名或移走，再继续下面的步骤。
        </div>
      )}

      {/* 表单一直挂在 DOM 里（不在当前步时隐藏），
          这样口令与邮箱不跨步丢失，也不需要把它们放进 React state。 */}
      <div className={[s.stepBody, step === 0 ? undefined : s.hidden].filter(Boolean).join(" ")}>
        <div className={s.sectionHead}>
          <span className={s.sectionNo}>①</span>
          <div>
            <div className={s.sectionTitle}>管理员账户</div>
            <div className={s.sectionSub}>用于解锁本机数据</div>
          </div>
        </div>

        <Field
          label="管理员邮箱（同时作为默认发件人）"
          htmlFor="s01-email"
          required
          error={
            emailLen > 0 && !emailOk
              ? "邮箱格式不对：要形如 name@company.com，@ 前后都不能是空的"
              : undefined
          }
        >
          <Input
            id="s01-email"
            ref={emailRef}
            type="email"
            placeholder="hr@company.com"
            autoComplete="username"
            onChange={revalidate}
            invalid={emailLen > 0 && !emailOk}
          />
        </Field>

        <Field
          label={`登录密码（至少 ${MIN_PASSWORD_CHARS} 位）`}
          htmlFor="s01-pw"
          required
          error={
            pwLen > 0 && !passwordOk
              ? `登录密码至少 ${MIN_PASSWORD_CHARS} 位，当前 ${pwLen} 位`
              : undefined
          }
        >
          <PasswordInput
            id="s01-pw"
            ref={pwRef}
            autoComplete="new-password"
            onChange={revalidate}
          />
        </Field>

        <div className={s.strength} aria-label="密码强度">
          {[0, 1, 2, 3].map((i) => (
            <i key={i} className={i < score ? s.strengthOn : undefined} />
          ))}
          <span className={s.strengthText}>
            {pwLen > 0 ? STRENGTH_TEXT[score] : "密码强度"}
          </span>
        </div>

        <Field
          label="确认密码"
          htmlFor="s01-pw2"
          required
          error={pw2Len > 0 && !same ? "两次输入的密码不一致" : undefined}
        >
          <PasswordInput
            id="s01-pw2"
            ref={pw2Ref}
            autoComplete="new-password"
            onChange={revalidate}
          />
        </Field>

        <Field label="单位名称（用于邮件模板默认值）" htmlFor="s01-org">
          <Input id="s01-org" ref={orgRef} placeholder="例如：某某科技有限公司" />
        </Field>

        <p className={s.finePrint}>
          密码仅保存在本机并经 Argon2id 加盐哈希；忘记密码无法远程找回，请自行妥善保管。
        </p>
      </div>

      {step === 1 && (
        <div className={s.stepBody}>
          <div className={s.sectionHead}>
            <span className={s.sectionNo}>②</span>
            <div>
              <div className={s.sectionTitle}>配置发件邮箱</div>
              <div className={s.sectionSub}>用于发送工资条</div>
            </div>
          </div>
          <div className={s.infoCard}>
            <div className={s.infoTitle}>发件邮箱稍后在设置页配置</div>
            <p className={s.infoText}>
              这一步需要真实邮箱的账号与专用密码才能「测试连接」。它不影响本机初始化，
              你可以先跳过，等发件账号到位后在「设置 → 邮件配置」里补上。
            </p>
            <p className={s.infoText}>跳过不影响任何已有数据，随时回来配置即可。</p>
          </div>
        </div>
      )}

      {step === 2 && (
        <div className={s.stepBody}>
          <div className={s.sectionHead}>
            <span className={s.sectionNo}>③</span>
            <div>
              <div className={s.sectionTitle}>完成引导</div>
              <div className={s.sectionSub}>这台机器将初始化为发件机</div>
            </div>
          </div>
          <div className={s.infoCard}>
            <div className={s.infoTitle}>即将创建管理员</div>
            <p className={s.infoText}>
              完成后这台电脑会用管理员口令锁住本机数据；每次启动、休眠恢复或空闲到点
              都要解锁才能进入工作台。
            </p>
          </div>
          {formError && (
            <div className={s.formError} role="alert">
              {formError}
            </div>
          )}
          {isPreview && (
            <p className={s.previewNote}>浏览器预览模式：这里不会真的建档。</p>
          )}
        </div>
      )}

      <div className={s.infoCard}>
        <div className={s.infoTitle}>数据仅保存在本机</div>
        <p className={s.infoText}>
          工资数据、成员名单、模板全部存储在本机加密数据库（SQLCipher），凭据进入 macOS 钥匙串。
        </p>
        <ul className={s.checkList}>
          <li>无云端同步</li>
          <li>无遥测 / 无崩溃上报</li>
          <li>除 SMTP 发信外不外联</li>
          <li>可随时导出或彻底清空</li>
        </ul>
        {dataDir && (
          <p className={s.dataDir}>
            数据目录 <code>{dataDir}</code>
          </p>
        )}
      </div>

      <div className={s.foot}>
        <span className={s.footLeft}>
          第 {step + 1} / 3 步
        </span>
        <span className={s.footSpacer} />
        {step === 0 && (
          <>
            <Button variant="ghost" onClick={() => setStep(1)}>
              稍后设置
            </Button>
            <Button variant="primary" disabled={!canNext} onClick={() => setStep(1)}>
              下一步：配置邮箱 →
            </Button>
          </>
        )}
        {step === 1 && (
          <>
            <Button variant="ghost" onClick={() => setStep(0)}>
              上一步
            </Button>
            <Button variant="primary" onClick={() => setStep(2)}>
              下一步：完成引导 →
            </Button>
          </>
        )}
        {step === 2 && (
          <Button variant="primary" loading={busy} onClick={finish}>
            完成引导，进入工作台
          </Button>
        )}
      </div>

      <p className={s.version}>薪邮 v{version}</p>
    </div>
  );
}
