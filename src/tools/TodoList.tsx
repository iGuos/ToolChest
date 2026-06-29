import { useState, useEffect, useRef, useMemo, useCallback } from "react";
import { useEscToClose } from "../hooks";

interface Task {
  id: number;
  groupId: number;
  text: string;
  done: boolean;
  createdAt: number;
  completedAt?: number;
}

interface Group {
  id: number;
  name: string;
}

interface Store {
  groups: Group[];
  tasks: Task[];
  activeGroupId: number;
}

const STORAGE_KEY = "baibao.todos.v2";
const LEGACY_KEY = "baibao.todos.v1";
const DEFAULT_GROUP: Group = { id: 1, name: "默认" };

function sanitizeTask(t: any, fallbackGroup: number): Task | null {
  if (!t || typeof t.text !== "string" || typeof t.id !== "number") return null;
  return {
    id: t.id,
    groupId: typeof t.groupId === "number" ? t.groupId : fallbackGroup,
    text: t.text,
    done: !!t.done,
    createdAt: typeof t.createdAt === "number" ? t.createdAt : Date.now(),
    completedAt: typeof t.completedAt === "number" ? t.completedAt : undefined,
  };
}

// 读取并归一化存储；v1（扁平任务数组）会被迁移进「默认」分组，老数据不丢。
function loadStore(): Store {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const d = JSON.parse(raw);
      if (d && Array.isArray(d.groups) && Array.isArray(d.tasks)) {
        const groups: Group[] = d.groups
          .filter((g: any) => g && typeof g.id === "number" && typeof g.name === "string")
          .map((g: any) => ({ id: g.id, name: g.name }));
        if (groups.length === 0) groups.push(DEFAULT_GROUP);
        const first = groups[0].id;
        const tasks = d.tasks
          .map((t: any) => sanitizeTask(t, first))
          .filter(Boolean) as Task[];
        const active = groups.some((g) => g.id === d.activeGroupId)
          ? d.activeGroupId
          : first;
        return { groups, tasks, activeGroupId: active };
      }
    }
  } catch {
    /* 落到迁移 / 全新 */
  }

  // 迁移 v1
  try {
    const legacy = localStorage.getItem(LEGACY_KEY);
    if (legacy) {
      const arr = JSON.parse(legacy);
      if (Array.isArray(arr)) {
        const tasks = arr
          .map((t: any) => sanitizeTask(t, DEFAULT_GROUP.id))
          .filter(Boolean) as Task[];
        return { groups: [DEFAULT_GROUP], tasks, activeGroupId: DEFAULT_GROUP.id };
      }
    }
  } catch {
    /* 全新 */
  }

  return { groups: [DEFAULT_GROUP], tasks: [], activeGroupId: DEFAULT_GROUP.id };
}

function fmt(ts?: number): string {
  if (!ts) return "";
  return new Date(ts).toLocaleString("zh", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

type Tab = "todo" | "done";

export default function TodoList() {
  // 只在挂载时读取一次，避免每个 useState 初始化都读 localStorage
  const bootRef = useRef<Store | null>(null);
  if (bootRef.current === null) bootRef.current = loadStore();
  const boot = bootRef.current;

  const [groups, setGroups] = useState<Group[]>(boot.groups);
  const [tasks, setTasks] = useState<Task[]>(boot.tasks);
  const [activeGroupId, setActiveGroupId] = useState<number>(boot.activeGroupId);

  const [tab, setTab] = useState<Tab>("todo");
  const [input, setInput] = useState("");
  const [editingId, setEditingId] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const [pendingDelete, setPendingDelete] = useState<Task | null>(null);
  const [pendingGroupDelete, setPendingGroupDelete] = useState<Group | null>(null);
  useEscToClose(!!pendingDelete, () => setPendingDelete(null));
  useEscToClose(!!pendingGroupDelete, () => setPendingGroupDelete(null));
  const [groupDeleteInput, setGroupDeleteInput] = useState("");

  // 分组下拉
  const [menuOpen, setMenuOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [newGroupName, setNewGroupName] = useState("");
  const [renamingId, setRenamingId] = useState<number | null>(null); // 正在重命名的分组
  const [renameInput, setRenameInput] = useState("");

  const taskIdRef = useRef(boot.tasks.reduce((m, t) => Math.max(m, t.id), 0) + 1);
  const groupIdRef = useRef(boot.groups.reduce((m, g) => Math.max(m, g.id), 0) + 1);

  const addRef = useRef<HTMLInputElement>(null);
  const editRef = useRef<HTMLInputElement>(null);

  // 持久化
  useEffect(() => {
    try {
      localStorage.setItem(
        STORAGE_KEY,
        JSON.stringify({ groups, tasks, activeGroupId })
      );
    } catch {
      /* 存储不可用时静默 */
    }
  }, [groups, tasks, activeGroupId]);

  useEffect(() => {
    if (editingId != null) editRef.current?.focus();
  }, [editingId]);

  // 打开下拉后，点击别处关闭（容器内点击会 stopPropagation，不会误触发）
  useEffect(() => {
    if (!menuOpen) return;
    const close = () => {
      setMenuOpen(false);
      setCreating(false);
    };
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [menuOpen]);

  const add = useCallback(() => {
    const text = input.trim();
    if (!text) return;
    setTasks((ts) => [
      ...ts,
      {
        id: taskIdRef.current++,
        groupId: activeGroupId,
        text,
        done: false,
        createdAt: Date.now(),
      },
    ]);
    setInput("");
    addRef.current?.focus();
  }, [input, activeGroupId]);

  const toggle = (id: number) =>
    setTasks((ts) =>
      ts.map((t) =>
        t.id === id
          ? { ...t, done: !t.done, completedAt: !t.done ? Date.now() : undefined }
          : t
      )
    );

  const remove = (id: number) => {
    setTasks((ts) => ts.filter((t) => t.id !== id));
    if (editingId === id) setEditingId(null);
  };

  // 删除：未完成的直接删；已完成的弹自定义弹窗二次确认。
  // 不用原生 confirm()——它在 Tauri WebView 里会静默返回，导致「点了没反应」。
  const requestRemove = (t: Task) => {
    if (t.done) setPendingDelete(t);
    else remove(t.id);
  };

  const confirmDelete = () => {
    if (pendingDelete) remove(pendingDelete.id);
    setPendingDelete(null);
  };

  const startEdit = (t: Task) => {
    setEditingId(t.id);
    setDraft(t.text);
  };

  const commitEdit = () => {
    if (editingId == null) return;
    const text = draft.trim();
    if (text) {
      setTasks((ts) => ts.map((t) => (t.id === editingId ? { ...t, text } : t)));
    }
    setEditingId(null);
  };

  const switchGroup = (id: number) => {
    setActiveGroupId(id);
    setMenuOpen(false);
    setCreating(false);
    setRenamingId(null);
    setEditingId(null);
    setPendingDelete(null);
  };

  const createGroup = () => {
    const name = newGroupName.trim();
    if (!name) return;
    const id = groupIdRef.current++;
    setGroups((gs) => [...gs, { id, name }]);
    setActiveGroupId(id);
    setNewGroupName("");
    setCreating(false);
    setMenuOpen(false);
    setEditingId(null);
  };

  // 重命名分组：点铅笔进入行内编辑，回车/✓ 保存
  const startRename = (g: Group) => {
    setRenamingId(g.id);
    setRenameInput(g.name);
    setCreating(false);
  };
  const commitRename = () => {
    const name = renameInput.trim();
    if (renamingId != null && name) {
      setGroups((gs) => gs.map((x) => (x.id === renamingId ? { ...x, name } : x)));
    }
    setRenamingId(null);
    setRenameInput("");
  };

  // 删除分组：需在弹框中输入分组名一致才放行；连同该组下全部任务一并删除。
  const openGroupDelete = (g: Group) => {
    setPendingGroupDelete(g);
    setGroupDeleteInput("");
    setMenuOpen(false);
    setCreating(false);
  };

  const confirmGroupDelete = () => {
    const g = pendingGroupDelete;
    if (!g || groupDeleteInput.trim() !== g.name) return;
    setTasks((ts) => ts.filter((t) => t.groupId !== g.id));
    setGroups((gs) => gs.filter((x) => x.id !== g.id));
    if (activeGroupId === g.id) {
      const remaining = groups.filter((x) => x.id !== g.id);
      setActiveGroupId(remaining[0]?.id ?? DEFAULT_GROUP.id);
    }
    setPendingGroupDelete(null);
    setGroupDeleteInput("");
    setEditingId(null);
  };

  const activeGroup =
    groups.find((g) => g.id === activeGroupId) ?? groups[0];

  const groupTasks = useMemo(
    () => tasks.filter((t) => t.groupId === activeGroupId),
    [tasks, activeGroupId]
  );
  const todos = useMemo(() => groupTasks.filter((t) => !t.done), [groupTasks]);
  const dones = useMemo(
    () =>
      groupTasks
        .filter((t) => t.done)
        .sort((a, b) => (b.completedAt ?? 0) - (a.completedAt ?? 0)),
    [groupTasks]
  );
  const list = tab === "todo" ? todos : dones;

  return (
    <div className="tool-container">
      <div className="tool-header">
        <div className="todo-title-row">
          <h2>待办事项</h2>
          <div className="group-select" onClick={(e) => e.stopPropagation()}>
            <button
              className="group-trigger"
              onClick={() => setMenuOpen((o) => !o)}
              title="切换 / 新建分组"
            >
              <span className="group-trigger-name">{activeGroup?.name ?? "默认"}</span>
              <span className="caret">▾</span>
            </button>
            {menuOpen && (
              <div className="group-menu">
                {groups.map((g) => {
                  const deletable = g.id !== DEFAULT_GROUP.id;
                  if (renamingId === g.id) {
                    // 行内重命名
                    return (
                      <div key={g.id} className="group-create" onClick={(e) => e.stopPropagation()}>
                        <input
                          autoFocus
                          value={renameInput}
                          placeholder="分组名称"
                          onChange={(e) => setRenameInput(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") commitRename();
                            else if (e.key === "Escape") {
                              setRenamingId(null);
                              setRenameInput("");
                            }
                          }}
                        />
                        <button className="group-create-ok" onClick={commitRename} disabled={!renameInput.trim()}>
                          ✓
                        </button>
                      </div>
                    );
                  }
                  return (
                    <div
                      key={g.id}
                      className={`group-item${g.id === activeGroupId ? " active" : ""}`}
                      onClick={() => switchGroup(g.id)}
                    >
                      <span className="group-item-name">{g.name}</span>
                      <span className="group-item-right">
                        {g.id === activeGroupId && <span className="group-check">✓</span>}
                        <button
                          className="group-del"
                          title="重命名分组"
                          aria-label="重命名分组"
                          onClick={(e) => {
                            e.stopPropagation();
                            startRename(g);
                          }}
                        >
                          <svg
                            viewBox="0 0 24 24"
                            width="13"
                            height="13"
                            fill="none"
                            stroke="currentColor"
                            strokeWidth="2"
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            aria-hidden="true"
                          >
                            <path d="M12 20h9" />
                            <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z" />
                          </svg>
                        </button>
                        {deletable && (
                          <button
                            className="group-del"
                            title="删除分组"
                            aria-label="删除分组"
                            onClick={(e) => {
                              e.stopPropagation();
                              openGroupDelete(g);
                            }}
                          >
                            <svg
                              viewBox="0 0 24 24"
                              width="13"
                              height="13"
                              fill="none"
                              stroke="currentColor"
                              strokeWidth="2"
                              strokeLinecap="round"
                              strokeLinejoin="round"
                              aria-hidden="true"
                            >
                              <path d="M3 6h18" />
                              <path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2" />
                              <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
                              <path d="M10 11v6" />
                              <path d="M14 11v6" />
                            </svg>
                          </button>
                        )}
                      </span>
                    </div>
                  );
                })}
                <div className="group-menu-sep" />
                {creating ? (
                  <div className="group-create">
                    <input
                      autoFocus
                      value={newGroupName}
                      placeholder="新分组名称"
                      onChange={(e) => setNewGroupName(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") createGroup();
                        else if (e.key === "Escape") setCreating(false);
                      }}
                    />
                    <button
                      className="group-create-ok"
                      onClick={createGroup}
                      disabled={!newGroupName.trim()}
                    >
                      ✓
                    </button>
                  </div>
                ) : (
                  <button
                    className="group-item group-add"
                    onClick={() => {
                      setCreating(true);
                      setNewGroupName("");
                    }}
                  >
                    + 新建分组
                  </button>
                )}
              </div>
            )}
          </div>
        </div>

        <div className="tool-actions">
          <div className="seg-toggle">
            <button
              className={`seg${tab === "todo" ? " active" : ""}`}
              onClick={() => setTab("todo")}
            >
              待办{todos.length ? ` (${todos.length})` : ""}
            </button>
            <button
              className={`seg${tab === "done" ? " active" : ""}`}
              onClick={() => setTab("done")}
            >
              已完成{dones.length ? ` (${dones.length})` : ""}
            </button>
          </div>
        </div>
      </div>

      {tab === "todo" && (
        <div className="todo-add">
          <input
            ref={addRef}
            className="todo-input"
            placeholder="添加一条计划，回车保存…"
            value={input}
            autoFocus
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") add();
            }}
          />
          <button className="btn btn-primary" onClick={add} disabled={!input.trim()}>
            添加
          </button>
        </div>
      )}

      <div className="todo-list">
        {list.length === 0 ? (
          <div className="empty">
            <div className="empty-icon">{tab === "todo" ? "📋" : "🎉"}</div>
            <div>
              {tab === "todo"
                ? "暂无待办，添加一条计划开始吧"
                : "还没有完成的事项"}
            </div>
          </div>
        ) : (
          list.map((t) => (
            <div key={t.id} className={`todo-item${t.done ? " done" : ""}`}>
              <button
                className={`todo-check${t.done ? " checked" : ""}`}
                onClick={() => toggle(t.id)}
                title={t.done ? "标记为未完成" : "标记为完成"}
              >
                {t.done ? "✓" : ""}
              </button>

              {editingId === t.id ? (
                <input
                  ref={editRef}
                  className="todo-edit"
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onBlur={commitEdit}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") commitEdit();
                    else if (e.key === "Escape") setEditingId(null);
                  }}
                />
              ) : (
                <span
                  className="todo-text"
                  title={t.done ? "" : "双击编辑"}
                  onDoubleClick={() => !t.done && startEdit(t)}
                >
                  {t.text}
                </span>
              )}

              <span className="todo-time">
                {t.done ? `完成于 ${fmt(t.completedAt)}` : fmt(t.createdAt)}
              </span>

              <div className="todo-row-actions">
                {!t.done && editingId !== t.id && (
                  <button
                    className="todo-icon-btn"
                    onClick={() => startEdit(t)}
                    title="编辑"
                  >
                    ✎
                  </button>
                )}
                <button
                  className="todo-icon-btn danger"
                  onClick={() => requestRemove(t)}
                  title="删除"
                  aria-label="删除"
                >
                  <svg
                    viewBox="0 0 24 24"
                    width="15"
                    height="15"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                  >
                    <path d="M3 6h18" />
                    <path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2" />
                    <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
                    <path d="M10 11v6" />
                    <path d="M14 11v6" />
                  </svg>
                </button>
              </div>
            </div>
          ))
        )}
      </div>

      <div className="tool-footer">
        <span>
          {todos.length} 条待办 · {dones.length} 条已完成
        </span>
        {groupTasks.length > 0 && (
          <span className="dim" style={{ marginLeft: "auto" }}>
            完成率 {Math.round((dones.length / groupTasks.length) * 100)}%
          </span>
        )}
      </div>

      {pendingDelete && (
        <div className="modal-overlay">
          <div className="modal" style={{ width: "min(420px, 90%)" }}>
            <div className="modal-head">
              <h3>删除已完成事项</h3>
              <button className="modal-close" onClick={() => setPendingDelete(null)}>×</button>
            </div>
            <p style={{ fontSize: 13, color: "var(--text)", lineHeight: 1.6 }}>
              确认删除「{pendingDelete.text}」？此操作不可撤销。
            </p>
            <div className="modal-actions">
              <button className="btn btn-ghost" onClick={() => setPendingDelete(null)}>
                取消
              </button>
              <button className="btn btn-danger" onClick={confirmDelete}>
                删除
              </button>
            </div>
          </div>
        </div>
      )}

      {pendingGroupDelete && (
        <div className="modal-overlay">
          <div className="modal" style={{ width: "min(440px, 90%)" }}>
            <div className="modal-head">
              <h3>删除分组</h3>
              <button className="modal-close" onClick={() => setPendingGroupDelete(null)}>×</button>
            </div>
            <p style={{ fontSize: 13, color: "var(--text)", lineHeight: 1.7 }}>
              将永久删除分组「{pendingGroupDelete.name}」及其下全部待办与已完成事项，
              <span style={{ color: "var(--red)" }}>此操作不可撤销</span>。
            </p>
            <p style={{ fontSize: 13, color: "var(--text-dim)", margin: 0 }}>
              请输入分组名称 <b style={{ color: "var(--text-hi)" }}>{pendingGroupDelete.name}</b> 以确认：
            </p>
            <input
              className="todo-input"
              autoFocus
              value={groupDeleteInput}
              placeholder={pendingGroupDelete.name}
              onChange={(e) => setGroupDeleteInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") confirmGroupDelete();
                else if (e.key === "Escape") setPendingGroupDelete(null);
              }}
            />
            <div className="modal-actions">
              <button className="btn btn-ghost" onClick={() => setPendingGroupDelete(null)}>
                取消
              </button>
              <button
                className="btn btn-danger"
                onClick={confirmGroupDelete}
                disabled={groupDeleteInput.trim() !== pendingGroupDelete.name}
              >
                删除分组
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
