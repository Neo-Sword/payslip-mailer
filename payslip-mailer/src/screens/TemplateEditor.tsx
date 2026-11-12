import { useCallback, useEffect, useRef, useState } from "react";
import { Button, Card, Chip, Field, Input } from "../components/ui";
import { api, BackendError } from "../lib/api";
import s from "./TemplateEditor.module.css";

/* ============================================================
   S10 · 中文邮件模板编辑器（TC-202 · ADR-0017）
   「变量插入 + 实时预览」

   三条硬规则：
   · **变量面板里的变量点一下就插入**，而且插入的一定是白名单里的
     （面板直接来自 `template_variables`，不另列一份）。
   · **未解析的变量高亮**，且**不让设为默认**（设计文档 S10 交互要点）。
   · 模板里写的是**字段的 key**（`{{字段.base_salary}}`），显示的是显示名 ——
     改个显示名不会让老模板坏掉（ADR-0017 决策 3）。
   ============================================================ */

export interface TemplateRow {
  id: number;
  name: string;
  subjectTpl: string;
  bodyHtml: string;
  bodyText: string;
  isDefault: boolean;
}

export interface VariableInfo {
  name: string;
  label: string;
  required: boolean;
  kind: "builtin" | "field";
}

export function TemplateEditor() {
  const [templates, setTemplates] = useState<TemplateRow[]>([]);
  const [variables, setVariables] = useState<VariableInfo[]>([]);
  const [current, setCurrent] = useState<TemplateRow | null>(null);
  const [subject, setSubject] = useState("");
  const [body, setBody] = useState("");
  const [preview, setPreview] = useState<{ subject: string; html: string; text: string } | null>(null);
  const [unknown, setUnknown] = useState<{ variable: string; message: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const bodyRef = useRef<HTMLTextAreaElement>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listTemplates();
      setTemplates(list);
      setVariables(await api.templateVariables());
      return list;
    } catch (raw) {
      setError((raw as BackendError).message || "读模板失败。");
      return [];
    }
  }, []);

  useEffect(() => {
    void (async () => {
      const list = await refresh();
      const first = list.find((t) => t.isDefault) ?? list[0];
      if (first) select(first);
    })();
  }, [refresh]);

  const select = (t: TemplateRow) => {
    setCurrent(t);
    setSubject(t.subjectTpl);
    setBody(t.bodyHtml);
    setError(null);
    void runPreview(t.subjectTpl, t.bodyHtml);
  };

  /** 实时预览 + 变量校验 */
  const runPreview = async (sub: string, html: string) => {
    try {
      setPreview(await api.previewTemplate(sub, html));
      const check = await api.checkTemplateVariables(sub, html);
      setUnknown(check.unknown);
    } catch (raw) {
      // 渲染失败（未知变量）→ 预览清空并把问题列出来
      setPreview(null);
      const e = raw as BackendError;
      setUnknown(e.code === "TEMPLATE_ERROR" ? [{ variable: "", message: e.message }] : []);
    }
  };

  /** 变量面板点一下 → 插入光标处 */
  const insertVariable = (name: string) => {
    const token = `{{${name}}}`;
    const el = bodyRef.current;
    if (!el) {
      setBody((b) => b + token);
      return;
    }
    const start = el.selectionStart ?? body.length;
    const end = el.selectionEnd ?? body.length;
    const next = body.slice(0, start) + token + body.slice(end);
    setBody(next);
    void runPreview(subject, next);
    requestAnimationFrame(() => {
      el.focus();
      const pos = start + token.length;
      el.setSelectionRange(pos, pos);
    });
  };

  const save = async () => {
    if (!current) return;
    setBusy(true);
    setError(null);
    try {
      await api.updateTemplate(current.id, current.name, subject, body);
      setNotice("已保存");
      const list = await refresh();
      const again = list.find((t) => t.id === current.id);
      if (again) select(again);
    } catch (raw) {
      setError((raw as BackendError).message || "保存失败。");
    } finally {
      setBusy(false);
    }
  };

  const setDefault = async () => {
    if (!current) return;
    setBusy(true);
    setError(null);
    try {
      await api.setDefaultTemplate(current.id);
      setNotice("已设为默认模板");
      await refresh();
    } catch (raw) {
      setError((raw as BackendError).message || "设为默认失败。");
    } finally {
      setBusy(false);
    }
  };

  const duplicate = async () => {
    if (!current) return;
    setBusy(true);
    setError(null);
    try {
      const copy = await api.duplicateTemplate(current.id);
      setNotice(`已存为副本：${copy.name}`);
      const list = await refresh();
      const again = list.find((t) => t.id === copy.id);
      if (again) select(again);
    } catch (raw) {
      setError((raw as BackendError).message || "存副本失败。");
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!current) return;
    setBusy(true);
    setError(null);
    try {
      await api.deleteTemplate(current.id);
      setNotice("已删除");
      const list = await refresh();
      const first = list.find((t) => t.isDefault) ?? list[0];
      if (first) select(first);
      else setCurrent(null);
    } catch (raw) {
      setError((raw as BackendError).message || "删除失败。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={s.wrap}>
      <Card
        title="邮件模板"
        meta={`${templates.length} 个 · 默认：${templates.find((t) => t.isDefault)?.name ?? "无"}`}
        className={s.listCard}
      >
        <div className={s.templateList}>
          {templates.map((t) => (
            <button
              key={t.id}
              type="button"
              className={[s.templateItem, current?.id === t.id ? s.templateItemOn : ""]
                .filter(Boolean)
                .join(" ")}
              onClick={() => select(t)}
            >
              <span>{t.name}</span>
              {t.isDefault && <Chip tone="ok">默认</Chip>}
            </button>
          ))}
        </div>
      </Card>

      {current && (
        <div className={s.editor}>
          <Card
            title={current.name}
            meta={current.isDefault ? "默认模板" : undefined}
            actions={
              <>
                <Button size="sm" onClick={duplicate}>
                  存为副本
                </Button>
                <Button size="sm" variant="ghost" onClick={remove}>
                  删除
                </Button>
                <Button size="sm" onClick={save} loading={busy}>
                  保存草稿
                </Button>
                <Button
                  size="sm"
                  variant="primary"
                  onClick={setDefault}
                  loading={busy}
                  disabled={unknown.length > 0}
                >
                  设为默认模板
                </Button>
              </>
            }
            className={s.editCard}
          >
            {unknown.length > 0 && (
              <div className={s.problemBox} role="alert">
                <div className={s.problemTitle}>这些变量解析不了，不能设为默认：</div>
                {unknown.map((p, i) => (
                  <div key={i} className={s.problemItem}>
                    {p.message}
                  </div>
                ))}
              </div>
            )}

            <Field label="邮件主题（支持变量）" htmlFor="tpl-subject">
              <Input
                id="tpl-subject"
                value={subject}
                onChange={(e) => {
                  setSubject(e.target.value);
                  void runPreview(e.target.value, body);
                }}
              />
            </Field>

            <Field label="正文 HTML" htmlFor="tpl-body" hint="变量面板点一下就插入光标处。">
              <textarea
                id="tpl-body"
                ref={bodyRef}
                className={s.bodyArea}
                rows={14}
                value={body}
                onChange={(e) => {
                  setBody(e.target.value);
                  void runPreview(subject, e.target.value);
                }}
              />
            </Field>

            <div className={s.varPanel}>
              <div className={s.varTitle}>可插入变量（点击插入光标处）</div>
              <div className={s.varList}>
                {variables.map((v) => (
                  <button
                    key={v.name}
                    type="button"
                    className={s.varChip}
                    onClick={() => insertVariable(v.name)}
                    title={v.name}
                  >
                    {v.label}
                    {v.required && <span className={s.varRequired}>必填</span>}
                  </button>
                ))}
              </div>
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
          </Card>

          <Card title="实时预览 · 以「张三」为例" className={s.previewCard}>
            <div className={s.previewNote}>示例数据 —— 真实的工资数据要等数据模块做好。</div>
            {preview ? (
              <>
                <div className={s.previewMeta}>
                  收件人：zhangsan@company.com
                  <br />
                  发件人：人力资源部 &lt;hr@company.com&gt;
                </div>
                <div className={s.previewSubject}>{preview.subject}</div>
                {/* 预览用 sandbox iframe，不直接 dangerouslySetInnerHTML：
                    模板里可能有 <script>，直接注入会让它在 webview 里执行 ——
                    而 webview 有 Tauri 的能力（dialog:allow-open 等）。
                    iframe 加 sandbox 后脚本被禁，与邮件客户端的行为一致。 */}
                <iframe
                  className={s.previewFrame}
                  title="邮件预览"
                  sandbox=""
                  srcDoc={preview.html}
                />
                <details className={s.textFallback}>
                  <summary>纯文本兜底版本</summary>
                  <pre className={s.textBody}>{preview.text}</pre>
                </details>
                <div className={s.okLine}>✓ 变量全部可解析 · 已自动生成 text/plain 兜底版本</div>
              </>
            ) : (
              <div className={s.emptyPreview}>预览不可用 —— 上面列出了解析不了的变量。</div>
            )}
          </Card>
        </div>
      )}
    </div>
  );
}
