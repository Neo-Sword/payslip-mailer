import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, Field, Input, Modal, Select, Switch } from "../components/ui";
import { api, BackendError } from "../lib/api";
import s from "./FieldConfig.module.css";

/* ============================================================
   S04 · 工资条项目配置（TC-200 · ADR-0015）
   「单一事实源：决定 Excel 表头 / 表格列 / 模板变量」

   三条硬规则（ADR-0015）：
   · **没有删除**。不要的字段「停用」它 —— 它不进新表头了，
     但历史数据里那个值还在，老模板也照样渲染得出来。
   · **key 不可改**：它已经进过表头与 `{{key}}`，改它等于换了一个字段。
   · 公式是**轻量校验**（引用的 key 存在、括号配平、字符集合法），
     真正的求值是 TC-203 的事。
   ============================================================ */

export interface FieldRow {
  id: number;
  key: string;
  label: string;
  dataType: "text" | "number" | "money" | "percent" | "formula";
  sortOrder: number;
  required: boolean;
  visible: boolean;
  formula: string | null;
  align: string;
}

const TYPES = [
  { value: "money", label: "金额" },
  { value: "number", label: "数字" },
  { value: "percent", label: "百分比" },
  { value: "text", label: "文本" },
  { value: "formula", label: "公式" },
] as const;

export function FieldConfig() {
  const [fields, setFields] = useState<FieldRow[]>([]);
  const [header, setHeader] = useState<string[]>([]);
  const [editing, setEditing] = useState<FieldRow | null>(null);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setFields(await api.listFields());
      setHeader(await api.previewHeader());
    } catch (raw) {
      setError((raw as BackendError).message || "读字段列表失败。");
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /** 上移 / 下移：改完立刻落库（拖拽的键盘等价物，也是排序的唯一入口） */
  const move = async (index: number, delta: number) => {
    const next = [...fields];
    const target = index + delta;
    if (target < 0 || target >= next.length) return;
    [next[index], next[target]] = [next[target], next[index]];
    setFields(next);
    try {
      await api.reorderFields(next.map((f) => f.id));
      setHeader(await api.previewHeader());
      setNotice(null);
    } catch (raw) {
      setError((raw as BackendError).message || "排序没有保存成功。");
      await refresh();
    }
  };

  const toggleVisible = async (row: FieldRow) => {
    try {
      await api.setFieldVisible(row.id, !row.visible);
      await refresh();
      setError(null);
    } catch (raw) {
      setError((raw as BackendError).message || "操作失败。");
    }
  };

  return (
    <div className={s.wrap}>
      <Card
        title="工资条项目"
        meta={`共 ${fields.length} 项`}
        actions={
          <Button size="sm" variant="primary" onClick={() => setCreating(true)}>
            ＋ 新增项目
          </Button>
        }
      >
        <div className={s.hint}>
          单一事实源：这一张表决定 Excel 表头、表格列与模板变量。
          <b>不要的字段请「停用」而不是删除</b> —— 停用后它不再出现在新导出的模板里，
          历史数据保持原样。
        </div>

        <div className={s.defaultNote}>
          初始的几项来自设计文档的示例清单（基本工资 / 绩效 / 餐补 / 社保 / 公积金 /
          实发工资 / 备注），<b>请按贵司实际情况增删停用</b>。项目与数据都在你自己的机器上。
        </div>

        {fields.length === 0 ? (
          <div className={s.empty}>
            还没有工资条项目。点右上角「＋ 新增项目」开始配置你要发的工资条长什么样。
          </div>
        ) : (
          <div className={s.table}>
            <div className={s.head}>
              <span className={s.colHandle} />
              <span className={s.colLabel}>显示名称</span>
              <span className={s.colKey}>变量键</span>
              <span className={s.colType}>类型</span>
              <span className={s.colBool}>必填</span>
              <span className={s.colBool}>显示</span>
              <span className={s.colFormula}>公式 / 说明</span>
              <span className={s.colOps}>操作</span>
            </div>
            {fields.map((f, i) => (
              <div key={f.id} className={s.row + (f.visible ? "" : " " + s.rowOff)}>
                <span className={s.colHandle}>
                  <button
                    type="button"
                    className={s.moveBtn}
                    onClick={() => void move(i, -1)}
                    disabled={i === 0}
                    aria-label={`把 ${f.label} 上移`}
                  >
                    ↑
                  </button>
                  <button
                    type="button"
                    className={s.moveBtn}
                    onClick={() => void move(i, 1)}
                    disabled={i === fields.length - 1}
                    aria-label={`把 ${f.label} 下移`}
                  >
                    ↓
                  </button>
                </span>
                <span className={s.colLabel}>{f.label}</span>
                <span className={s.colKey}>{f.key}</span>
                <span className={s.colType}>
                  <Chip tone={f.dataType === "formula" ? "accent" : "neutral"}>
                    {TYPES.find((t) => t.value === f.dataType)?.label ?? f.dataType}
                  </Chip>
                </span>
                <span className={s.colBool}>{f.required ? "是" : "—"}</span>
                <span className={s.colBool}>
                  <Switch
                    checked={f.visible}
                    onChange={() => void toggleVisible(f)}
                    label={`${f.label} 是否出现在新模板里`}
                  />
                </span>
                <span className={s.colFormula}>{f.formula ?? "—"}</span>
                <span className={s.colOps}>
                  <Button size="sm" variant="ghost" onClick={() => setEditing(f)}>
                    编辑
                  </Button>
                </span>
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
      </Card>

      <Card title="预览表头">
        <div className={s.hint}>
          下一份空模板会出现的列 —— 就是下面这些（停用的字段不在里面）。
        </div>
        {header.length === 0 ? (
          <div className={s.empty}>还没有可用的字段。</div>
        ) : (
          <div className={s.headerPreview}>
            {header.map((h) => (
              <span key={h} className={s.headerCell}>
                {h}
              </span>
            ))}
          </div>
        )}
      </Card>

      <FieldDialog
        open={creating || editing !== null}
        row={editing}
        busy={busy}
        setBusy={setBusy}
        onClose={() => {
          setCreating(false);
          setEditing(null);
        }}
        onSaved={async () => {
          setCreating(false);
          setEditing(null);
          setError(null);
          await refresh();
        }}
        onError={setError}
      />
    </div>
  );
}

/* ---------------- 新增 / 编辑 ---------------- */

function FieldDialog({
  open,
  row,
  busy,
  setBusy,
  onClose,
  onSaved,
  onError,
}: {
  open: boolean;
  row: FieldRow | null;
  busy: boolean;
  setBusy: (b: boolean) => void;
  onClose: () => void;
  onSaved: () => void;
  onError: (m: string | null) => void;
}) {
  const isEdit = row !== null;
  const [key, setKey] = useState("");
  const [label, setLabel] = useState("");
  const [dataType, setDataType] = useState<string>("money");
  const [required, setRequired] = useState(false);
  const [formulaError, setFormulaError] = useState<string | null>(null);
  const [calcResult, setCalcResult] = useState<string | null>(null);
  const [calcBusy, setCalcBusy] = useState(false);
  const [formula, setFormula] = useState("");
  const [touched, setTouched] = useState(false);

  useEffect(() => {
    if (!open) return;
    setKey(row?.key ?? "");
    setLabel(row?.label ?? "");
    setDataType(row?.dataType ?? "money");
    setRequired(row?.required ?? false);
    setFormula(row?.formula ?? "");
    setTouched(false);
    onError(null);
  }, [open, row, onError]);

  const isFormula = dataType === "formula";
  // key 的本地校验（与服务端同一套规则的简化版）：小写字母 / 数字 / 下划线
  const keyLooksOk = /^[a-z_][a-z0-9_]*$/.test(key) && key.length <= 40;
  /** 试算：用示例值算一遍，看这条公式算得出来不。 */
  const runCalc = async () => {
    setCalcBusy(true);
    setFormulaError(null);
    setCalcResult(null);
    try {
      const values: Record<string, string> = {
        base_salary: "18000",
        performance: "3200",
        meal_allowance: "400",
        social_insurance: "-2100",
        housing_fund: "-1800",
      };
      const result = await api.evaluateFormula(formula, values, 21600, -3900);
      setCalcResult(result);
    } catch (raw) {
      setFormulaError((raw as BackendError).message || "这条公式算不出来。");
    } finally {
      setCalcBusy(false);
    }
  };

  const canSave = label.trim().length > 0 && (isEdit || keyLooksOk) && (!isFormula || formula.trim().length > 0);

  const save = async () => {
    setTouched(true);
    if (!canSave) return;
    setBusy(true);
    try {
      if (isEdit && row) {
        await api.updateField(
          row.id,
          label.trim(),
          dataType,
          required,
          isFormula && formula.trim() ? formula.trim() : null,
        );
      } else {
        await api.createField(
          key.trim(),
          label.trim(),
          dataType,
          required,
          isFormula && formula.trim() ? formula.trim() : null,
        );
      }
      onSaved();
    } catch (raw) {
      onError((raw as BackendError).message || "保存失败。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      title={isEdit ? "编辑项目" : "新增项目"}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" loading={busy} disabled={!canSave} onClick={save}>
            保存
          </Button>
        </>
      }
    >
      <div className={s.dialogBody}>
        <Field label="显示名称" htmlFor="fld-label" required>
          <Input
            id="fld-label"
            value={label}
            placeholder="例如：基本工资"
            onChange={(e) => setLabel(e.target.value)}
          />
        </Field>

        <Field
          label="变量键（英文）"
          htmlFor="fld-key"
          required={!isEdit}
          hint={
            isEdit
              ? "变量键不可改：它已经进过表头与模板变量，改了等于换了一个字段。"
              : "只能是小写字母、数字和下划线；模板里写 {{key}}，Excel 里是列名。"
          }
          error={
            !isEdit && touched && key.length > 0 && !keyLooksOk
              ? "变量键只能是小写字母、数字和下划线，且不能以数字开头。"
              : undefined
          }
        >
          <Input
            id="fld-key"
            value={key}
            disabled={isEdit}
            placeholder="base_salary"
            onChange={(e) => setKey(e.target.value)}
          />
        </Field>

        <Field label="类型" htmlFor="fld-type" required>
          <Select
            id="fld-type"
            value={dataType}
            onChange={(e) => setDataType(e.target.value)}
          >
            {TYPES.map((t) => (
              <option key={t.value} value={t.value}>
                {t.label}
              </option>
            ))}
          </Select>
        </Field>

        {isFormula && (
          <Field
            label="公式表达式"
            htmlFor="fld-formula"
            required
            hint="只支持字段名（base_salary）、汇总变量（应发合计 / 扣款合计）与 + − × ÷ ( )。"
            error={formulaError ?? undefined}
          >
            <Input
              id="fld-formula"
              value={formula}
              placeholder="应发合计 + 扣款合计"
              onChange={(e) => {
                setFormula(e.target.value);
                setFormulaError(null);
                setCalcResult(null);
              }}
            />
          </Field>
        )}

        {isFormula && formula.trim().length > 0 && (
          <div className={s.calcRow}>
            <Button size="sm" onClick={runCalc} loading={calcBusy}>
              试算
            </Button>
            {calcResult !== null && <span className={s.calcOk}>算出来是 {calcResult}</span>}
            {calcResult === null && formulaError && (
              <span className={s.calcErr}>{formulaError}</span>
            )}
          </div>
        )}

        <div className={s.requiredRow}>
          <Switch checked={required} onChange={setRequired} label="必填" />
          <span className={s.requiredText}>必填（导入时这一列不能空）</span>
        </div>

        <p className={s.note}>
          右侧实时生效，保存即写入本机加密库。没有「删除」—— 不要的字段请在上表里
          <b>停用</b>它。
        </p>
      </div>
    </Modal>
  );
}
