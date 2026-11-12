import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { Button, Card, Chip, Select, Steps } from "../components/ui";
import {
  api,
  BackendError,
  type ColumnMapping,
  type ColumnTarget,
  type ConflictStrategy,
  type ImportPreview,
  type ImportSummary,
  type PeriodRow,
  type RowProblem,
} from "../lib/api";
import s from "./ImportWizard.module.css";

/* ============================================================
   S08 · Excel 导入向导（TC-302 · ADR-0021）
   上传文件 → 表头映射 → 校验与入库

   三条要紧的口径：

   1. **原文件只读打开、不复制**（设计文档 ch.9）——
      用户在系统文件对话框里选文件，我们只在后端读它一遍。
   2. **K>0 时禁止入库**（ch.8「结果预览」）：
      有错误行就不让入库，先改好 Excel 再导。
   3. **警告不拦人，但拦发送**：姓名与成员表不一致的那几行会入库，
      同时带「身份待核对」标记 —— 它们**不会进入发送范围**（TC-401 负责拦）。
   ============================================================ */

type Step = "upload" | "mapping" | "commit";

const STEP_ITEMS: { label: string; state: "pending" | "active" | "done" }[] = [
  { label: "上传文件", state: "pending" },
  { label: "表头映射", state: "pending" },
  { label: "校验与入库", state: "pending" },
];

const MATCH_LABEL: Record<string, string> = {
  exact: "精确",
  fuzzy: "模糊",
  unmatched: "未匹配",
};

const MATCH_TONE: Record<string, "ok" | "warn" | "danger"> = {
  exact: "ok",
  fuzzy: "warn",
  unmatched: "danger",
};

const STRATEGY_LABEL: Record<ConflictStrategy, string> = {
  overwrite: "覆盖同月已有数据（生成变更审计）",
  fillEmpty: "仅补充空白单元格",
  cancel: "取消导入",
};

export function ImportWizard() {
  const [periods, setPeriods] = useState<PeriodRow[]>([]);
  const [periodId, setPeriodId] = useState<number | null>(null);
  const [step, setStep] = useState<Step>("upload");
  const [path, setPath] = useState<string | null>(null);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  /** 用户改过的映射（按列序索引）。没改过的列用后端给的那份。 */
  const [overrides, setOverrides] = useState<Record<number, ColumnTarget>>({});
  const [strategy, setStrategy] = useState<ConflictStrategy | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  /** 生效的映射 = 后端自动映射 + 用户改过的。 */
  const mappings = useMemo<ColumnMapping[]>(() => {
    if (!preview) return [];
    return preview.mappings.map((m) =>
      overrides[m.index] ? { ...m, target: overrides[m.index], needsConfirmation: false } : m,
    );
  }, [preview, overrides]);

  /** 表头映射里有哪些目标可选（固定列 + 工资字段 + 跳过）。 */
  const targetOptions = useMemo(() => {
    const opts: { value: ColumnTarget; label: string }[] = [];
    for (const m of preview?.mappings ?? []) {
      const t = m.target;
      if (t.kind === "fixed") opts.push({ value: t, label: `固定列 · ${t.key}` });
      if (t.kind === "field") opts.push({ value: t, label: `字段 · ${t.key}` });
    }
    opts.push({ value: { kind: "skip" }, label: "跳过这一列" });
    return opts;
  }, [preview]);

  const refreshPeriods = useCallback(async () => {
    try {
      const list = await api.listPeriods();
      setPeriods(list);
      setPeriodId((prev) => {
        if (prev !== null && list.some((p) => p.id === prev)) return prev;
        return list[0]?.id ?? null;
      });
    } catch (raw) {
      setError((raw as BackendError).message || "读月份账套失败。");
    }
  }, []);

  useEffect(() => {
    void refreshPeriods();
  }, [refreshPeriods]);

  const pick = useCallback(async () => {
    setError(null);
    setNotice(null);
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "Excel", extensions: ["xlsx", "xls", "xlsb", "ods"] }],
      });
      if (typeof picked !== "string") return;
      if (periodId === null) {
        setError("先选一个月份账套，再导入这个月的工资数据。");
        return;
      }
      setBusy(true);
      const p = await api.previewImport(periodId, picked);
      setPath(picked);
      setPreview(p);
      setOverrides({});
      setStrategy(null);
      setStep("mapping");
    } catch (raw) {
      setError((raw as BackendError).message || "读不了这个文件，请换一个试试。");
    } finally {
      setBusy(false);
    }
  }, [periodId]);

  /** 换映射后重新预览（校验结果随映射变）。 */
  const revalidate = useCallback(
    async (next: Record<number, ColumnTarget>) => {
      if (!path || periodId === null || !preview) return;
      setBusy(true);
      setError(null);
      try {
        const merged = preview.mappings.map((m) =>
          next[m.index] ? { ...m, target: next[m.index], needsConfirmation: false } : m,
        );
        const p = await api.previewImport(periodId, path);
        // 后端重新算的映射为准（用户的改动手工合并上去）
        const mergedIdx = new Map(merged.map((m) => [m.index, m.target]));
        setPreview({
          ...p,
          mappings: p.mappings.map((m) => ({
            ...m,
            target: mergedIdx.get(m.index) ?? m.target,
          })),
        });
        setOverrides(next);
      } catch (raw) {
        setError((raw as BackendError).message || "校验失败，请重试。");
      } finally {
        setBusy(false);
      }
    },
    [path, periodId, preview],
  );

  const changeTarget = (index: number, value: string) => {
    const target: ColumnTarget =
      value === "skip"
        ? { kind: "skip" }
        : value.startsWith("fixed:")
          ? { kind: "fixed", key: value.slice("fixed:".length) }
          : { kind: "field", key: value.slice("field:".length) };
    const next = { ...overrides, [index]: target };
    setOverrides(next);
    void revalidate(next);
  };

  const targetValue = (t: ColumnTarget): string =>
    t.kind === "skip" ? "skip" : `${t.kind}:${t.key}`;

  const doCommit = async () => {
    if (!path || periodId === null) return;
    if (strategy === null) {
      setError("请先选择导入策略。");
      return;
    }
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const outcome = await api.commitImport(periodId, path, mappings, strategy);
      if (outcome.cancelled) {
        setNotice("已取消导入，本月的数据没有任何改动。");
        return;
      }
      const unverified =
        outcome.identityUnverified > 0
          ? `；其中 ${outcome.identityUnverified} 行「身份待核对」，不会进入发送范围`
          : "";
      setNotice(
        `已导入 ${outcome.written} 行` +
          (outcome.replaced > 0 ? `（覆盖了 ${outcome.replaced} 行）` : "") +
          unverified,
      );
      setStep("upload");
      setPath(null);
      setPreview(null);
      setOverrides({});
      setStrategy(null);
    } catch (raw) {
      setError((raw as BackendError).message || "导入失败，请重试。");
    } finally {
      setBusy(false);
    }
  };

  const downloadErrors = async () => {
    if (!path || periodId === null) return;
    setBusy(true);
    try {
      const out = await api.exportImportErrors(periodId, path, mappings);
      setNotice(`错误明细已导出：${out}`);
    } catch (raw) {
      setError((raw as BackendError).message || "导出错误明细失败。");
    } finally {
      setBusy(false);
    }
  };

  const steps = STEP_ITEMS.map((item, i) => ({
    label: item.label,
    state:
      (i === 0 && step !== "upload") || (i === 1 && step === "commit")
        ? ("done" as const)
        : i === ["upload", "mapping", "commit"].indexOf(step)
          ? ("active" as const)
          : ("pending" as const),
  }));

  const summary: ImportSummary = preview?.summary ?? {
    success: 0,
    warnings: 0,
    errors: 0,
  };
  const errorProblems = (preview?.problems ?? []).filter((p) => p.severity === "error");
  const warningProblems = (preview?.problems ?? []).filter((p) => p.severity === "warning");
  const canCommit =
    summary.errors === 0 &&
    preview !== null &&
    preview.missingFixed.length === 0 &&
    !preview.needsConfirmation;

  return (
    <div className={s.wrap}>
      <Steps items={steps} />

      {step === "upload" && (
        <Card
          title="导入这个月的工资数据"
          meta="支持 .xlsx / .xls / .xlsb / .ods"
          actions={
            <Button variant="primary" loading={busy} disabled={periodId === null} onClick={pick}>
              ⇧ 选择文件…
            </Button>
          }
        >
          <div className={s.periodRow}>
            <span className={s.periodLabel}>导入到</span>
            <Select
              value={periodId ?? ""}
              onChange={(e) => setPeriodId(Number(e.target.value))}
            >
              {periods.length === 0 && <option value="">（还没有月份账套）</option>}
              {periods.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.yearMonth} · {p.status === "draft" ? "草稿" : "已准备"}
                </option>
              ))}
            </Select>
          </div>
          <p className={s.note}>
            原文件不会被复制进应用目录 —— 只在本机读一遍，解析完就写进加密库。
          </p>
          {periodId === null && (
            <p className={s.warn}>先在「月度工资表」里新建一个月份账套，再导入这个月的工资数据。</p>
          )}
        </Card>
      )}

      {step === "mapping" && preview && (
        <>
          <Card
            title="表头映射"
            meta={`${preview.fileName} · ${preview.rowCount} 行数据 · ${preview.mappings.length} 列`}
            actions={
              <Button onClick={pick} disabled={busy}>
                重新选择文件
              </Button>
            }
          >
            {preview.missingFixed.length > 0 && (
              <p className={s.error}>
                还差这几列没对上：{preview.missingFixed.join("、")}。
                没有邮箱就找不到人，没有姓名就看不出是谁 —— 这三列必须各自映射到一列。
              </p>
            )}
            {preview.needsConfirmation && (
              <p className={s.warn}>
                有模糊匹配的列，请逐列确认映射是否正确。
              </p>
            )}
            <div className={s.mapTable}>
              <MappingTable
                mappings={mappings}
                targetOptions={targetOptions}
                onChange={changeTarget}
                targetValue={targetValue}
              />
            </div>
            {preview.mappings.some((m) => m.matchKind === "unmatched") && (
              <p className={s.warn}>
                未匹配的列已被跳过，那一列的数据不会进库。
                若这一列需要保留，请先在「工资条项目」里加上它，再重新导入。
              </p>
            )}
          </Card>

          <Card title="校验结果">
            <div className={s.counts}>
              <Count label="成功" value={summary.success} tone="ok" />
              <Count label="警告" value={summary.warnings} tone="warn" />
              <Count label="错误" value={summary.errors} tone="danger" />
            </div>
            {errorProblems.length > 0 && (
              <>
                <p className={s.error}>
                  有 {summary.errors} 行数据有问题，没有导入。请按下面的说明改好 Excel 后重新导入。
                </p>
                <ProblemList problems={errorProblems} />
                <div className={s.actions}>
                  <Button onClick={downloadErrors} disabled={busy}>
                    下载错误明细
                  </Button>
                </div>
              </>
            )}
            {warningProblems.length > 0 && (
              <>
                <p className={s.warn}>
                  有 {summary.warnings} 行带提醒，可以导入，但请先核对。
                </p>
                <ProblemList problems={warningProblems} />
              </>
            )}
            {errorProblems.length === 0 && warningProblems.length === 0 && (
              <p className={s.note}>没有发现问题。</p>
            )}
          </Card>

          <Card title="导入策略">
            <p className={s.note}>
              这个月已经有数据时，导入策略决定怎么处理。
            </p>
            <div className={s.strategies}>
              {(Object.keys(STRATEGY_LABEL) as ConflictStrategy[]).map((key) => (
                <label key={key} className={s.strategy}>
                  <input
                    type="radio"
                    name="import-strategy"
                    checked={strategy === key}
                    onChange={() => setStrategy(key)}
                  />
                  <span>{STRATEGY_LABEL[key]}</span>
                </label>
              ))}
            </div>
            <div className={s.actions}>
              <Button variant="ghost" onClick={() => setStep("upload")}>
                上一步
              </Button>
              <Button
                variant="primary"
                loading={busy}
                disabled={!canCommit || strategy === null}
                onClick={doCommit}
              >
                入库
              </Button>
            </div>
          </Card>
        </>
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
    </div>
  );
}

/* ---------------- 表格 ---------------- */

function MappingTable({
  mappings,
  targetOptions,
  onChange,
  targetValue,
}: {
  mappings: ColumnMapping[];
  targetOptions: { value: ColumnTarget; label: string }[];
  onChange: (index: number, value: string) => void;
  targetValue: (t: ColumnTarget) => string;
}) {
  return (
    <table className={s.table}>
      <thead>
        <tr>
          <th>Excel 列名</th>
          <th>映射到</th>
          <th>识别</th>
        </tr>
      </thead>
      <tbody>
        {mappings.map((m) => (
          <tr key={m.index}>
            <td>{m.header || "（空列名）"}</td>
            <td>
              <select
                className={s.select}
                value={targetValue(m.target)}
                onChange={(e) => onChange(m.index, e.target.value)}
              >
                {targetOptions.map((o) => (
                  <option key={targetValue(o.value)} value={targetValue(o.value)}>
                    {o.label}
                  </option>
                ))}
              </select>
            </td>
            <td>
              <Chip tone={MATCH_TONE[m.matchKind]}>{MATCH_LABEL[m.matchKind]}</Chip>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function ProblemList({ problems }: { problems: RowProblem[] }) {
  return (
    <ul className={s.problems}>
      {problems.slice(0, 50).map((p, i) => (
        <li key={`${p.row}-${p.column}-${i}`}>
          <span className={s.problemRow}>第 {p.row} 行</span>
          <span className={s.problemCol}>{p.column}</span>
          <span>{p.message}</span>
        </li>
      ))}
      {problems.length > 50 && (
        <li className={s.note}>还有 {problems.length - 50} 条，下载错误明细看全部。</li>
      )}
    </ul>
  );
}

function Count({
  label,
  value,
  tone,
}: {
  label: string;
  value: number;
  tone: "ok" | "warn" | "danger";
}) {
  return (
    <span className={[s.count, s[`count_${tone}`]].filter(Boolean).join(" ")}>
      <b>{value}</b>
      <span>{label}</span>
    </span>
  );
}
