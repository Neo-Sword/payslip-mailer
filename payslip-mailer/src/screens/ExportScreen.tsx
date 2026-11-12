import { useCallback, useEffect, useState } from "react";
import { Card, Select } from "../components/ui";
import { api, BackendError, type PeriodRow } from "../lib/api";
import { ExportPanel } from "./ExportPanel";
import s from "./PeriodPanel.module.css";

/* ============================================================
   S09 的独立入口（TC-301 · ADR-0020）

   为什么单独一屏：侧边栏有一项「导出空模板」，但它原来**没有对应界面**
   （点进去落在占位页），真正的导出面板藏在「月度工资表」那一屏里 ——
   侧边栏承诺的屏幕必须存在，不然就是一条死路。

   这一屏只做一件事：**选月份 → 导出**。
   导出面板与「月度工资表」里用的是**同一个** ExportPanel，
   表头预览与导出文件里的表头同源（ADR-0020）。
   ============================================================ */

export function ExportScreen() {
  const [periods, setPeriods] = useState<PeriodRow[]>([]);
  const [currentId, setCurrentId] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listPeriods();
      setPeriods(list);
      setCurrentId((prev) => {
        if (prev !== null && list.some((p) => p.id === prev)) return prev;
        return list[0]?.id ?? null;
      });
      setError(null);
    } catch (raw) {
      setError((raw as BackendError).message || "读月份账套失败。");
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className={s.wrap}>
      <Card title="导出空模板">
        {periods.length === 0 ? (
          <div className={s.empty}>
            还没有月份账套。先到「月度工资表」点「＋ 新建月份」建一个（比如 2026-12），
            回到这里就能导出那个月的空模板了。
          </div>
        ) : (
          <div className={s.bar}>
            <Select
              aria-label="选择要导出的月份"
              value={currentId ?? ""}
              onChange={(e) => setCurrentId(Number(e.target.value))}
            >
              {periods.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.yearMonth}
                </option>
              ))}
            </Select>
            <span className={s.readonlyNote}>
              导出的表头是<b>那个月</b>的列：草稿用当前的工资条项目配置，进入「待发送」之后就冻结了。
            </span>
          </div>
        )}
        {error && (
          <div className={s.error} role="alert">
            {error}
          </div>
        )}
      </Card>

      {currentId !== null && <ExportPanel periodId={currentId} />}
    </div>
  );
}
