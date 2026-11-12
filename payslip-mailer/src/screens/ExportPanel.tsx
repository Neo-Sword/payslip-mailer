import { useCallback, useEffect, useState } from "react";
import { Button, Card, Switch } from "../components/ui";
import { api, BackendError } from "../lib/api";
import s from "./ExportPanel.module.css";

/* ============================================================
   S09 · 按配置导出空白 Excel 模板（TC-301 · ADR-0020）
   「表头随 S04 字段配置动态生成」

   界面上的「表头预览」与导出文件里的表头**一字不差** ——
   两者调的是**同一个** `preview_template_header`（标准 11）。
   ============================================================ */

export interface SheetColumn {
  title: string;
  key: string;
  kind: "fixed" | "field";
  required: boolean;
  isFormula: boolean;
  numeric: boolean;
  dataType: string | null;
}

export function ExportPanel({ periodId }: { periodId: number | null }) {
  const [columns, setColumns] = useState<SheetColumn[]>([]);
  const [includeInstructions, setIncludeInstructions] = useState(true);
  const [includeSample, setIncludeSample] = useState(false);
  const [includeMembers, setIncludeMembers] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (periodId === null) {
      setColumns([]);
      return;
    }
    try {
      setColumns((await api.previewTemplateHeader(periodId)).columns);
      setError(null);
    } catch (raw) {
      setError((raw as BackendError).message || "读表头失败。");
    }
  }, [periodId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const doExport = async () => {
    if (periodId === null) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const r = await api.exportTemplate(
        periodId,
        includeInstructions,
        includeSample,
        includeMembers,
      );
      setNotice(`已导出：${r.path}`);
    } catch (raw) {
      setError((raw as BackendError).message || "导出失败，请重试。");
    } finally {
      setBusy(false);
    }
  };

  const fixedCount = columns.filter((c) => c.kind === "fixed").length;
  const fieldCount = columns.length - fixedCount;

  return (
    <div className={s.wrap}>
      <Card
        title="导出空模板"
        meta={columns.length > 0 ? `字段配置：${fieldCount} 项` : undefined}
        actions={
          <Button
            size="sm"
            variant="primary"
            loading={busy}
            disabled={periodId === null || columns.length === 0}
            onClick={doExport}
          >
            ⇩ 导出 .xlsx
          </Button>
        }
      >
        {periodId === null ? (
          <div className={s.empty}>先在上方选一个月份账套，再导出它的空模板。</div>
        ) : (
          <>
            <div className={s.opts}>
              <label className={s.opt}>
                <Switch
                  checked={includeInstructions}
                  onChange={setIncludeInstructions}
                  label="包含「填写说明」Sheet"
                />
                <span>包含「填写说明」Sheet</span>
              </label>
              <label className={s.opt}>
                <Switch
                  checked={includeSample}
                  onChange={setIncludeSample}
                  label="包含 2 行示例数据"
                />
                <span>包含 2 行示例数据</span>
              </label>
              <label className={s.opt}>
                <Switch
                  checked={includeMembers}
                  onChange={setIncludeMembers}
                  label="预填成员名单"
                />
                <span>预填成员名单（会把姓名与邮箱写进文件）</span>
              </label>
            </div>

            <div className={s.previewTitle}>
              表头预览
              <span className={s.previewHint}>
                {fixedCount} 个固定列 + {fieldCount} 个启用字段 = {columns.length} 列
              </span>
            </div>

            {columns.length === 0 ? (
              <div className={s.empty}>这个月还没有可用的工资条项目列。</div>
            ) : (
              <div className={s.cols}>
                {columns.map((c) => (
                  <span key={c.key} className={s.col}>
                    {c.title}
                    {c.required && <span className={s.colRequired}>*</span>}
                    {c.isFormula && <span className={s.colFormula}>公式</span>}
                  </span>
                ))}
              </div>
            )}

            <p className={s.note}>
              导出的表头是<b>那个月的列</b>：草稿用当前的工资条项目配置，
              进入「待发送」之后就冻结了 —— 后来改配置不影响这个月。
            </p>

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
          </>
        )}
      </Card>
    </div>
  );
}
