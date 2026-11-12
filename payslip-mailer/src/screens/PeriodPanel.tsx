import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, Select } from "../components/ui";
import { api, BackendError } from "../lib/api";
import { ExportPanel } from "./ExportPanel";
import { RecordsTable } from "./RecordsTable";
import s from "./PeriodPanel.module.css";

/* ============================================================
   S07 · 月度工资数据 —— 账套骨架（TC-300 · ADR-0019）
   本卡只做「账套」：月份切换 + 状态 + 这个月显示哪些列。
   记录本身（逐格编辑、合计行）是 TC-303；
   导出空模板是 TC-301；导入是 TC-302；前往预览核对是 TC-401。

   一条要紧的口径：**列从哪儿来取决于状态** ——
   草稿用当前的字段配置；进入 ready 起用**冻结的快照**
   （所以「改字段配置」不会弄乱已经准备发送的历史月份）。
   ============================================================ */

export interface PeriodRow {
  id: number;
  yearMonth: string;
  status: "draft" | "ready" | "sending" | "sent" | "archived";
  fieldSnapshot: string | null;
  lockedAt: number | null;
  /** 现在能不能编辑工资数值 */
  editable: boolean;
  readOnly: boolean;
}

export interface SnapshotField {
  key: string;
  label: string;
  dataType: string;
  formula: string | null;
  required: boolean;
  visible: boolean;
  sortOrder: number;
}

const STATUS_LABEL: Record<string, string> = {
  draft: "草稿",
  ready: "待发送",
  sending: "发送中",
  sent: "已发送",
  archived: "已归档",
};

const STATUS_TONE: Record<string, "neutral" | "info" | "warn" | "ok"> = {
  draft: "neutral",
  ready: "info",
  sending: "warn",
  sent: "ok",
  archived: "neutral",
};

/** `2026-10` → 「2026 年 10 月」 */
function formatYearMonth(ym: string): string {
  const [y, m] = ym.split("-");
  return `${y} 年 ${Number(m)} 月`;
}

export function PeriodPanel({ onDataChanged }: { onDataChanged?: () => void }) {
  const [periods, setPeriods] = useState<PeriodRow[]>([]);
  const [currentId, setCurrentId] = useState<number | null>(null);
  const [fields, setFields] = useState<SnapshotField[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listPeriods();
      setPeriods(list);
      setCurrentId((prev) => {
        if (prev !== null && list.some((p) => p.id === prev)) return prev;
        return list[0]?.id ?? null;
      });
      onDataChanged?.();
    } catch (raw) {
      setError((raw as BackendError).message || "读月份账套失败。");
    }
  }, [onDataChanged]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (currentId === null) {
      setFields([]);
      return;
    }
    void api
      .getPeriodFields(currentId)
      .then(setFields)
      .catch(() => setFields([]));
  }, [currentId]);

  const current = periods.find((p) => p.id === currentId) ?? null;

  const create = async (ym: string) => {
    setBusy(true);
    setError(null);
    try {
      const p = await api.createPeriod(ym);
      setNotice(`已建 ${formatYearMonth(p.yearMonth)} 的账套（草稿）`);
      await refresh();
      setCurrentId(p.id);
    } catch (raw) {
      setError((raw as BackendError).message || "建账套失败。");
    } finally {
      setBusy(false);
    }
  };

  /** 状态迁移。非法迁移由后端拒（且不留痕迹），这里只把原因显示出来。 */
  const move = async (target: string) => {
    if (currentId === null) return;
    setBusy(true);
    setError(null);
    try {
      const p = await api.transitionPeriod(currentId, target);
      setNotice(`已变成「${STATUS_LABEL[p.status] ?? p.status}」`);
      await refresh();
    } catch (raw) {
      setError((raw as BackendError).message || "状态没改成。");
    } finally {
      setBusy(false);
    }
  };

  const unlock = async () => {
    if (currentId === null) return;
    setBusy(true);
    setError(null);
    try {
      await api.unlockPeriod(currentId);
      setNotice("已解锁回到草稿，并留下一条修订记录");
      await refresh();
    } catch (raw) {
      setError((raw as BackendError).message || "解锁失败。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={s.wrap}>
      <Card
        title="月度工资表"
        actions={
          <>
            <Button size="sm" onClick={() => void create(newMonthCandidate())} loading={busy}>
              ＋ 新建月份
            </Button>
            <Button size="sm" onClick={() => void move("ready")} loading={busy} disabled={current?.status !== "draft"}>
              标记为待发送
            </Button>
            {current?.status === "sent" && (
              <Button size="sm" variant="danger" onClick={() => void unlock()} loading={busy}>
                解锁纠错
              </Button>
            )}
            <Button size="sm" variant="primary" disabled>
              前往预览核对 →
            </Button>
          </>
        }
      >
        <div className={s.bar}>
          <Select
            aria-label="切换月份"
            value={currentId ?? ""}
            onChange={(e) => setCurrentId(Number(e.target.value))}
          >
            {periods.length === 0 && <option value="">还没有月份账套</option>}
            {periods.map((p) => (
              <option key={p.id} value={p.id}>
                {formatYearMonth(p.yearMonth)}
              </option>
            ))}
          </Select>
          {current && (
            <Chip tone={STATUS_TONE[current.status] ?? "neutral"}>
              {STATUS_LABEL[current.status] ?? current.status}
            </Chip>
          )}
          {current?.readOnly && (
            <span className={s.readonlyNote}>已发送的账套只读 —— 要改得先「解锁纠错」。</span>
          )}
        </div>

        {current && (
          <>
            <div className={s.colsTitle}>
              这个月的列（{fields.length} 列）
              <span className={s.colsHint}>
                {current.status === "draft"
                  ? "草稿阶段用当前的工资条项目配置 —— 改了立刻生效。"
                  : "已冻结在进入「待发送」的那一刻 —— 之后改工资条项目配置不影响这个月。"}
              </span>
            </div>
            <div className={s.cols}>
              {fields
                .filter((f) => f.visible)
                .map((f) => (
                  <span key={f.key} className={s.col}>
                    {f.label}
                    {f.formula && <span className={s.colFormula}>公式</span>}
                  </span>
                ))}
            </div>
          </>
        )}

        {!current && (
          <div className={s.empty}>
            还没有月份账套。点「＋ 新建月份」建一个（比如 2026-10），这个月就能开始录数据了。
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
      </Card>

      {/* S07 的表格本体（TC-303）：只有这一屏渲染记录。
          列来源与上面一致（draft 用当前配置、ready 起用快照）。 */}
      {currentId !== null && (
        <RecordsTable periodId={currentId} editable={current?.editable ?? false} fields={fields} />
      )}

      <ExportPanel periodId={currentId} />
    </div>
  );
}

/** 建议的新建月份：当前月的下一月（按现在的年月 + 1）。 */
function newMonthCandidate(): string {
  const now = new Date();
  let y = now.getFullYear();
  let m = now.getMonth() + 2; // 下个月
  if (m > 12) {
    m = 1;
    y += 1;
  }
  return `${y}-${String(m).padStart(2, "0")}`;
}
