import { useCallback, useEffect, useState } from "react";
import { Button, Card, Chip, Field, Input, Modal, Switch } from "../components/ui";
import { api, BackendError } from "../lib/api";
import s from "./MemberConfig.module.css";

/* ============================================================
   S05 · 邮件组与成员（TC-201 · ADR-0016）
   「组列表 + 成员表格 / 分组群发的范围来源」

   三条硬规则：
   · **没有删除成员** —— 要让某人不再收到信 = 停用他（停用的人不进发送范围，
     但名单里的记录还在）。
   · **批量粘贴先预览**：重复的自动去掉、坏行明确指出第几行，
     用户看着预览确认了才真的写进去。
   · **邮箱全局唯一**：编辑表单里撞了别人会报错 + 字段下红描边。
   ============================================================ */

export interface MemberRow {
  id: number;
  name: string;
  email: string;
  employeeNo: string | null;
  active: boolean;
  note: string | null;
  groups: string[];
}

export interface GroupRow {
  id: number;
  name: string;
  note: string | null;
  sortOrder: number;
  activeMemberCount: number;
  memberCount: number;
}

interface PastePreviewData {
  accepted: { name: string; email: string }[];
  problems: { line: number; field: string; message: string }[];
  deduped: number;
  skippedHeader: boolean;
  delimiter: string;
}

export function MemberConfig() {
  const [members, setMembers] = useState<MemberRow[]>([]);
  const [groups, setGroups] = useState<GroupRow[]>([]);
  const [selected, setSelected] = useState<number[]>([]);
  const [editing, setEditing] = useState<MemberRow | null>(null);
  const [creating, setCreating] = useState(false);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [groupOpen, setGroupOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setMembers(await api.listMembers());
      setGroups(await api.listGroups());
    } catch (raw) {
      setError((raw as BackendError).message || "读名单失败。");
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const toggleActive = async (row: MemberRow) => {
    try {
      await api.setMemberActive(row.id, !row.active);
      setError(null);
      await refresh();
    } catch (raw) {
      setError((raw as BackendError).message || "操作失败。");
    }
  };

  const removeGroup = async (g: GroupRow) => {
    try {
      await api.deleteGroup(g.id);
      setNotice(`已删除「${g.name}」。组里的成员仍��名单里，只是不再属于任何组。`);
      setError(null);
      await refresh();
    } catch (raw) {
      setError((raw as BackendError).message || "删除组失败。");
    }
  };

  return (
    <div className={s.wrap}>
      <div className={s.columns}>
        {/* ---------------- 组列表 ---------------- */}
        <Card
          title="邮件组"
          meta={`${groups.length} 组`}
          actions={
            <Button size="sm" onClick={() => setGroupOpen(true)}>
              ＋ 新建
            </Button>
          }
          className={s.groupCard}
        >
          <div className={s.hint}>成员可属于多个组；停用成员不进入发送范围。</div>
          {groups.length === 0 ? (
            <div className={s.empty}>还没有邮件组。点「＋ 新建」建一个。</div>
          ) : (
            <div className={s.groupList}>
              {groups.map((g) => (
                <div key={g.id} className={s.groupItem}>
                  <span className={s.groupName}>{g.name}</span>
                  <span className={s.groupCount}>{g.activeMemberCount}</span>
                  <button
                    type="button"
                    className={s.groupDel}
                    onClick={() => void removeGroup(g)}
                    aria-label={`删除组 ${g.name}`}
                  >
                    ×
                  </button>
                </div>
              ))}
            </div>
          )}
        </Card>

        {/* ---------------- 成员表格 ---------------- */}
        <Card
          title="成员名单"
          meta={`${members.length} 位`}
          actions={
            <>
              <Button size="sm" onClick={() => setPasteOpen(true)}>
                批量粘贴
              </Button>
              <Button size="sm" variant="primary" onClick={() => setCreating(true)}>
                ＋ 添加成员
              </Button>
            </>
          }
          className={s.memberCard}
        >
          <div className={s.hint}>
            同一个邮箱在名单里只有一个人（重复粘贴会自动去重）。已选 {selected.length} / {members.length}
          </div>

          {members.length === 0 ? (
            <div className={s.empty}>
              名单还是空的。点「批量粘贴」把「姓名 邮箱」两列粘进来，或点「＋ 添加成员」一个个加。
            </div>
          ) : (
            <div className={s.table}>
              <div className={s.head}>
                <span className={s.colPick} />
                <span className={s.colName}>姓名</span>
                <span className={s.colEmail}>邮箱</span>
                <span className={s.colNo}>工号</span>
                <span className={s.colGroups}>所属组</span>
                <span className={s.colState}>状态</span>
                <span className={s.colOps}>操作</span>
              </div>
              {members.map((m) => (
                <div key={m.id} className={s.row + (m.active ? "" : " " + s.rowOff)}>
                  <span className={s.colPick}>
                    <input
                      type="checkbox"
                      checked={selected.includes(m.id)}
                      onChange={(e) =>
                        setSelected((prev) =>
                          e.target.checked
                            ? [...prev, m.id]
                            : prev.filter((x) => x !== m.id),
                        )
                      }
                      aria-label={`选中 ${m.name}`}
                    />
                  </span>
                  <span className={s.colName}>{m.name}</span>
                  <span className={s.colEmail}>{m.email}</span>
                  <span className={s.colNo}>{m.employeeNo ?? "—"}</span>
                  <span className={s.colGroups}>{m.groups.join("、") || "—"}</span>
                  <span className={s.colState}>
                    {m.active ? (
                      <Chip tone="ok">启用</Chip>
                    ) : (
                      <Chip tone="neutral">停用</Chip>
                    )}
                  </span>
                  <span className={s.colOps}>
                    <Button size="sm" variant="ghost" onClick={() => setEditing(m)}>
                      编辑
                    </Button>
                    <Button size="sm" variant="ghost" onClick={() => void toggleActive(m)}>
                      {m.active ? "停用" : "启用"}
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
      </div>

      <MemberDialog
        open={creating || editing !== null}
        row={editing}
        groups={groups}
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

      <PasteDialog
        open={pasteOpen}
        onClose={() => setPasteOpen(false)}
        onDone={async (n) => {
          setPasteOpen(false);
          setNotice(`已写入 ${n} 位成员。`);
          await refresh();
        }}
        onError={setError}
      />

      <GroupDialog
        open={groupOpen}
        onClose={() => setGroupOpen(false)}
        onDone={async () => {
          setGroupOpen(false);
          await refresh();
        }}
        onError={setError}
      />
    </div>
  );
}

/* ---------------- 成员表单 ---------------- */

function MemberDialog({
  open,
  row,
  groups,
  onClose,
  onSaved,
  onError,
}: {
  open: boolean;
  row: MemberRow | null;
  groups: GroupRow[];
  onClose: () => void;
  onSaved: () => void;
  onError: (m: string | null) => void;
}) {
  const isEdit = row !== null;
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [employeeNo, setEmployeeNo] = useState("");
  const [active, setActive] = useState(true);
  const [picked, setPicked] = useState<number[]>([]);
  const [emailError, setEmailError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setName(row?.name ?? "");
    setEmail(row?.email ?? "");
    setEmployeeNo(row?.employeeNo ?? "");
    setActive(row?.active ?? true);
    setPicked([]);
    setEmailError(null);
    onError(null);
  }, [open, row, onError]);

  /** 邮箱撞了别人？本地先查一遍（服务端也会再挡一次） */
  const checkEmailTaken = async () => {
    if (!email.includes("@")) {
      setEmailError(null);
      return;
    }
    try {
      const all = await api.listMembers();
      const hit = all.find((m) => m.email.toLowerCase() === email.trim().toLowerCase() && m.id !== row?.id);
      setEmailError(hit ? "这个邮箱已经在名单里了" : null);
    } catch {
      setEmailError(null);
    }
  };

  const save = async () => {
    setBusy(true);
    onError(null);
    try {
      if (isEdit && row) {
        await api.updateMember(row.id, name.trim(), email.trim(), employeeNo.trim() || null, picked);
      } else {
        await api.createMember(name.trim(), email.trim(), employeeNo.trim() || null, picked);
      }
      onSaved();
    } catch (raw) {
      onError((raw as BackendError).message || "保存失败。");
    } finally {
      setBusy(false);
    }
  };

  const canSave = name.trim().length > 0 && email.includes("@") && emailError === null;

  return (
    <Modal
      open={open}
      title={isEdit ? "编辑成员" : "添加成员"}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" loading={busy} disabled={!canSave} onClick={save}>
            保存成员
          </Button>
        </>
      }
    >
      <div className={s.dialogBody}>
        <Field label="姓名" htmlFor="m-name" required>
          <Input id="m-name" value={name} onChange={(e) => setName(e.target.value)} />
        </Field>

        <Field
          label="邮箱"
          htmlFor="m-email"
          required
          error={emailError ?? undefined}
          hint="同一个邮箱在名单里只有一个人。"
        >
          <Input
            id="m-email"
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            onBlur={() => void checkEmailTaken()}
          />
        </Field>

        <Field label="工号（用于 Excel 精确匹配）" htmlFor="m-no">
          <Input id="m-no" value={employeeNo} onChange={(e) => setEmployeeNo(e.target.value)} />
        </Field>

        <div className={s.pickGroups}>
          <div className={s.pickTitle}>所属邮件组</div>
          {groups.length === 0 ? (
            <div className={s.pickEmpty}>还没有邮件组。成员可以先不加组，之后再补。</div>
          ) : (
            <div className={s.pickList}>
              {groups.map((g) => (
                <label key={g.id} className={s.pickItem}>
                  <input
                    type="checkbox"
                    checked={picked.includes(g.id)}
                    onChange={(e) =>
                      setPicked((prev) =>
                        e.target.checked ? [...prev, g.id] : prev.filter((x) => x !== g.id),
                      )
                    }
                  />
                  <span>{g.name}</span>
                </label>
              ))}
            </div>
          )}
          {picked.length > 0 && (
            <button type="button" className={s.clearGroups} onClick={() => setPicked([])}>
              移出全部组
            </button>
          )}
        </div>

        <div className={s.activeRow}>
          <Switch checked={active} onChange={setActive} label="启用（参与发送）" />
          <span className={s.activeText}>启用（参与发送）</span>
        </div>

        <p className={s.note}>
          修改邮箱后，<b>历史发送日志仍保留原收件地址</b>，便于追溯。
          名单里的人只能「停用」，没有删除 —— 停用的人不在发送范围里，记录还在。
        </p>
      </div>
    </Modal>
  );
}

/* ---------------- 批量粘贴 ---------------- */

function PasteDialog({
  open,
  onClose,
  onDone,
  onError,
}: {
  open: boolean;
  onClose: () => void;
  onDone: (n: number) => void;
  onError: (m: string | null) => void;
}) {
  const [text, setText] = useState("");
  const [preview, setPreview] = useState<PastePreviewData | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setText("");
    setPreview(null);
    onError(null);
  }, [open, onError]);

  const runPreview = async () => {
    setBusy(true);
    try {
      setPreview(await api.previewMemberPaste(text));
      onError(null);
    } catch (raw) {
      onError((raw as BackendError).message || "预览失败。");
    } finally {
      setBusy(false);
    }
  };

  const doPaste = async () => {
    if (!preview) return;
    setBusy(true);
    try {
      const n = await api.pasteMembers(preview.accepted);
      onDone(n);
    } catch (raw) {
      onError((raw as BackendError).message || "写入失败。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      title="批量粘贴"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          {preview ? (
            <Button variant="primary" loading={busy} disabled={preview.accepted.length === 0} onClick={doPaste}>
              写入 {preview.accepted.length} 位
            </Button>
          ) : (
            <Button variant="primary" loading={busy} disabled={text.trim().length === 0} onClick={runPreview}>
              预览
            </Button>
          )}
        </>
      }
    >
      <div className={s.dialogBody}>
        <Field
          label="粘贴两列（姓名、邮箱）"
          htmlFor="paste-text"
          required
          hint="制表符分隔（从 Excel 复制）或逗号分隔都行。第一行若是表头会自动跳过。"
        >
          <textarea
            id="paste-text"
            className={s.pasteArea}
            rows={8}
            value={text}
            placeholder={"姓名\t邮箱\n张三\tzhangsan@company.com"}
            onChange={(e) => setText(e.target.value)}
          />
        </Field>

        {preview && (
          <div className={s.previewBox}>
            <div className={s.previewHead}>
              将写入 <b>{preview.accepted.length}</b> 位成员
              {preview.deduped > 0 && <>（去重 {preview.deduped} 条）</>}
              {preview.skippedHeader && <>（已跳过表头）</>}
              {preview.delimiter && <> · 用{preview.delimiter}分隔</>}
            </div>
            {preview.problems.length > 0 && (
              <div className={s.problemList}>
                {preview.problems.map((p, i) => (
                  <div key={i} className={s.problem}>
                    第 {p.line} 行 · {p.field}：{p.message}
                  </div>
                ))}
              </div>
            )}
            {preview.accepted.length > 0 && (
              <div className={s.acceptList}>
                {preview.accepted.map((a) => (
                  <div key={a.email} className={s.acceptItem}>
                    {a.name} · {a.email}
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}

/* ---------------- 新建组 ---------------- */

function GroupDialog({
  open,
  onClose,
  onDone,
  onError,
}: {
  open: boolean;
  onClose: () => void;
  onDone: () => void;
  onError: (m: string | null) => void;
}) {
  const [name, setName] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setName("");
    setNote("");
    onError(null);
  }, [open, onError]);

  const save = async () => {
    setBusy(true);
    try {
      await api.createGroup(name.trim(), note.trim() || null);
      onDone();
    } catch (raw) {
      onError((raw as BackendError).message || "建组失败。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      title="新建邮件组"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button variant="primary" loading={busy} disabled={name.trim().length === 0} onClick={save}>
            建组
          </Button>
        </>
      }
    >
      <div className={s.dialogBody}>
        <Field label="组名" htmlFor="g-name" required hint="组名唯一。">
          <Input id="g-name" value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="备注" htmlFor="g-note">
          <Input id="g-note" value={note} onChange={(e) => setNote(e.target.value)} />
        </Field>
        <p className={s.note}>
          删除组只会解除归属，<b>不会删掉成员</b>。
        </p>
      </div>
    </Modal>
  );
}
