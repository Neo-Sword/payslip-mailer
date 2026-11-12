import type { ReactNode } from "react";
import { TitleBar } from "./TitleBar";
import s from "./AppShell.module.css";

/* ============================================================
   应用骨架 · 对应原型 S03 工作台
   侧边栏固定 5 组，条目在各屏保持完全一致
   ============================================================ */

export interface NavItem {
  id: string;
  label: string;
  icon: string;
  badge?: number;
}

export interface NavGroup {
  label: string;
  items: NavItem[];
}

/** 侧边栏信息架构（设计文档 ch.11）*/
export const NAV_GROUPS: NavGroup[] = [
  { label: "工作台", items: [{ id: "overview", label: "概览", icon: "◧" }] },
  {
    label: "基础配置",
    items: [
      { id: "fields", label: "工资条项目", icon: "▦" },
      { id: "groups", label: "邮件组与成员", icon: "◉", badge: 3 },
      { id: "templates", label: "邮件模板", icon: "✎" },
    ],
  },
  {
    label: "工资数据",
    items: [
      { id: "periods", label: "月度工资表", icon: "▤" },
      { id: "import", label: "Excel 导入", icon: "⇧" },
      { id: "export", label: "导出空模板", icon: "⇩" },
    ],
  },
  {
    label: "发送",
    items: [
      { id: "preview", label: "预览与核对", icon: "➤" },
      { id: "queue", label: "发送队列", icon: "≡" },
      { id: "history", label: "发送历史", icon: "≡" },
    ],
  },
  {
    label: "系统",
    items: [
      { id: "settings", label: "设置", icon: "⚙" },
      { id: "security", label: "安全与备份", icon: "🔒" },
    ],
  },
];

export function AppShell({
  title,
  titleRight,
  active,
  onNavigate,
  collapsed,
  onToggleCollapse,
  footer,
  children,
}: {
  title: string;
  titleRight?: ReactNode;
  active: string;
  onNavigate: (id: string) => void;
  collapsed?: boolean;
  onToggleCollapse?: () => void;
  footer?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className={s.shell}>
      <TitleBar title={title} titleRight={titleRight} />

      <div className={s.body}>
        <nav className={[s.sidebar, collapsed ? s.sidebarCollapsed : undefined].filter(Boolean).join(" ")}>
          <div className={s.brand}>
            <span className={s.logo} aria-hidden>
              薪
            </span>
            {!collapsed && <span>薪邮 Payslip</span>}
          </div>

          {NAV_GROUPS.map((group) => (
            <div key={group.label}>
              {!collapsed && <div className={s.groupLabel}>{group.label}</div>}
              {group.items.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  className={[s.item, active === item.id ? s.itemActive : undefined]
                    .filter(Boolean)
                    .join(" ")}
                  onClick={() => onNavigate(item.id)}
                  title={collapsed ? item.label : undefined}
                >
                  <span className={s.itemIcon} aria-hidden>
                    {item.icon}
                  </span>
                  {!collapsed && <span className={s.itemLabel}>{item.label}</span>}
                  {!collapsed && item.badge !== undefined && (
                    <span className={s.itemBadge}>{item.badge}</span>
                  )}
                </button>
              ))}
            </div>
          ))}

          {!collapsed && <div className={s.sidebarFoot}>{footer}</div>}
          {onToggleCollapse && (
            <button
              type="button"
              className={s.item}
              onClick={onToggleCollapse}
              style={{ marginTop: 12 }}
              aria-label={collapsed ? "展开侧边栏" : "折叠侧边栏"}
            >
              <span className={s.itemIcon} aria-hidden>
                {collapsed ? "»" : "«"}
              </span>
              {!collapsed && <span className={s.itemLabel}>折叠</span>}
            </button>
          )}
        </nav>

        <main className={s.pane}>{children}</main>
      </div>
    </div>
  );
}

/** 内容区顶栏 */
export function PaneHead({
  title,
  sub,
  actions,
}: {
  title: ReactNode;
  sub?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className={s.paneHead}>
      <h2 className={s.paneTitle}>{title}</h2>
      {sub && <span className={s.paneSub}>{sub}</span>}
      <span className={s.paneSpacer} />
      {actions}
    </div>
  );
}
