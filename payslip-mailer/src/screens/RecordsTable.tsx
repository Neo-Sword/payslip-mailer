import { useCallback, useEffect, useMemo, useState } from "react";
import { Chip, ConfirmDialog, Empty } from "../components/ui";
import { api, BackendError, type RecordRow, type PeriodTotals, type SnapshotField } from "../lib/api";
import s from "./RecordsTable.module.css";

/* ============================================================
   S07 · 月度工资表（TC-303 · ADR-0022）
   逐格编辑 + 公式列重算 + 校验列 + 合计行

   口径：
   - 单格保存（失焦/回车触发），保存后整行重算 —— 返回的新行原地替换。
   - 公式列只读（前端禁用 + 后端拒绝，两层）。
   - 校验列：必填缺失 → 警告；**警告不拦保存，拦发送**（TC-401）。
   - 合计行 = 整月（v1 无分页）；公式列不进合计（ADR-0022 决策 4）。
   ============================================================ */

/** 千分位（与后端 format_amount 同一观感） */
function fmtAmount(v: string | undefined): string {
  if (v === undefined || v === "") return "";
  const n = Number(v);
  if (Number.isNaN(n)) return v;
  const [i, f] = n.toFixed(2).split(".");
  const grouped = i.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return n < 0 ? `-${grouped.replace("-", "")}.${f}` : `${grouped}.${f}`;
}

export function RecordsTable({
  periodId,
  editable,
  fields,
}: {
  periodId: number | null;
  editable: boolean;
  fields: SnapshotField[];
}) {
  const [rows, setRows] = useState<RecordRow[]>([]);
  const [totals, setTotals] = useState<PeriodTotals | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 正在编辑的格子（memberId + key）。null = 没在编辑。 */
  const [editing, setEditing] = useState<{ memberId: number; key: string; draft: string } | null>(
    null,
  );
  /** 待删的行（确认对话框）。 */
  const [pendingDelete, setPendingDelete] = useState<RecordRow | null>(null);

  const refresh = useCallback(async () => {
    if (periodId === null) {
      setRows([]);
      setTotals(null);
      return;
    }
    setBusy(true);
    try {
      const [list, t] = await Promise.all([
        api.listRecords(periodId),
        api.getPeriodTotals(periodId),
      ]);
      setRows(list);
      setTotals(t);
      setError(null);
    } catch (raw) {
      setError((raw as BackendError).message || "读工资记录失败。");
    } finally {
      setBusy(false);
    }
  }, [periodId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /** 保存一格：调后端 → 返回的新行原地替换（不整表刷新）。 */
  const saveCell = useCallback(
    async (memberId: number, key: string, value: string) => {
      if (periodId === null) return;
      setBusy(true);
      setError(null);
      try {
        const updated = await api.updateRecordCell(periodId, memberId, key, value);
        setRows((prev) => prev.map((r) => (r.memberId === memberId ? updated : r)));
        // 合计跟着变（改了任何一格都该重算）
        setTotals(await api.getPeriodTotals(periodId));
      } catch (raw) {
        setError((raw as BackendError).message || "保存失败。");
      } finally {
        setBusy(false);
      }
    },
    [periodId],
  );

  const doDelete = useCallback(async () => {
    if (periodId === null || pendingDelete === null) return;
    setBusy(true);
    try {
      await api.deleteRecord(periodId, pendingDelete.memberId);
      setPendingDelete(null);
      await refresh();
    } catch (raw) {
      setError((raw as BackendError).message || "删除失败。");
    } finally {
      setBusy(false);
    }
  }, [periodId, pendingDelete, refresh]);

  /** 数值列（可编辑）与公式列（只读）由字段定义决定。 */
  const columns = useMemo(() => fields.filter((f) => f.visible), [fields]);

  const problemCount = rows.filter((r) => r.hasProblem).length;

  if (periodId === null) {
    return null;
  }

  if (rows.length === 0) {
    return (
      <CardLike>
        <Empty
          icon="▤"
          title="这个月还没有工资数据"
          hint="先在「导出空模板」里把表交给财务填写，再到「Excel 导入」里导回来；或者直接在导入后逐格补数。"
        />
        {error && <div className={s.error}>{error}</div>}
      </CardLike>
    );
  }

  return (
    <div className={s.wrap}>
      <div className={s.meta}>
        共 <b>{rows.length}</b> 条记录 ·{" "}
        {problemCount > 0 ? (
          <>
            待处理 <b className={s.warnText}>{problemCount}</b>
          </>
        ) : (
          <>校验全部通过</>
        )}
        {busy && <span className={s.busy}>处理中…</span>}
      </div>

      <div className={s.tableBox}>
        <table className={s.table}>
          <thead>
            <tr>
              <th className={s.stickyCol}>姓名</th>
              <th>邮箱</th>
              {columns.map((f) => (
                <th key={f.key} className={s.numericHead}>
                  {f.label}
                  {f.required && <span className={s.req}>*</span>}
                  {f.dataType === "formula" && <span className={s.formulaTag}>公式</span>}
                </th>
              ))}
              <th>校验</th>
              {editable && <th aria-label="操作" />}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.memberId} className={row.hasProblem ? s.rowProblem : undefined}>
                <td className={s.stickyCol}>
                  <b>{row.name}</b>
                </td>
                <td className={s.emailCell}>{row.email}</td>
                {columns.map((f) => {
                  const isFormula = f.dataType === "formula";
                  const value = row.values[f.key] ?? "";
                  const editingThis =
                    editing !== null && editing.memberId === row.memberId && editing.key === f.key;
                  if (isFormula || !editable) {
                    return (
                      <td key={f.key} className={s.numeric}>
                        {f.dataType !== "text" ? fmtAmount(value) : value}
                        {value === "" && <span className={s.absent}>—</span>}
                      </td>
                    );
                  }
                  return (
                    <td key={f.key} className={s.numeric}>
                      {editingThis ? (
                        <input
                          className={s.cellInput}
                          value={editing.draft}
                          autoFocus
                          onChange={(e) =>
                            setEditing({ ...editing, draft: e.target.value })
                          }
                          onBlur={() => {
                            const draft = editing.draft;
                            setEditing(null);
                            if (draft !== value) void saveCell(row.memberId, f.key, draft);
                          }}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                            if (e.key === "Escape") setEditing(null);
                          }}
                        />
                      ) : (
                        <button
                          type="button"
                          className={s.cellButton}
                          onClick={() =>
                            setEditing({ memberId: row.memberId, key: f.key, draft: value })
                          }
                          title="点击编辑"
                        >
                          {f.dataType !== "text" ? fmtAmount(value) : value || "—"}
                        </button>
                      )}
                    </td>
                  );
                })}
                <td>
                  {row.validation === "" ? (
                    <Chip tone="ok">通过</Chip>
                  ) : (
                    <Chip tone="warn">{row.validation}</Chip>
                  )}
                </td>
                {editable && (
                  <td>
                    <button
                      type="button"
                      className={s.deleteBtn}
                      onClick={() => setPendingDelete(row)}
                      title="删除这一行"
                    >
                      删
                    </button>
                  </td>
                )}
              </tr>
            ))}
          </tbody>

        </table>
      </div>

      {totals && (
        <div className={s.totalsBar}>
          <span>
            应发 <b>{fmtAmount(totals.earnings.toFixed(2))}</b>
          </span>
          <span>
            扣款 <b>{fmtAmount(totals.deductions.toFixed(2))}</b>
          </span>
          <span>
            实发 <b>{fmtAmount(totals.net.toFixed(2))}</b>
          </span>
          <span className={s.totalsHint}>合计按整月、不重复计公式列</span>
        </div>
      )}

      {error && (
        <div className={s.error} role="alert">
          {error}
        </div>
      )}

      <ConfirmDialog
        open={pendingDelete !== null}
        title="删除这条工资记录"
        danger
        confirmText="删除这条记录"
        description={`将删除 ${pendingDelete?.name ?? ""}（${pendingDelete?.email ?? ""}）在这个月的全部工资数据。这一步不会删成员本身；重新导入或重新录入可以再建。`}
        onConfirm={() => void doDelete()}
        onCancel={() => setPendingDelete(null)}
      />
    </div>
  );
}

/** 没有标题的简单卡片容器（空状态用）。 */
function CardLike({ children }: { children: React.ReactNode }) {
  return <div className={s.emptyCard}>{children}</div>;
}
