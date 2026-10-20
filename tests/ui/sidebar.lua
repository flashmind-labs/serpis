local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local shots = os.getenv("SERPIS_TEST_SHOTS")

-- A throwaway repository.
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "/proj'")
sh("cd '" .. tmp .. "/proj' && git init -q -b main && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init")
local proj = sh("cd '" .. tmp .. "/proj' && pwd -P")
serpis.var.set("worktree-directory", tmp .. "/wt")

local function item(id)
  for _, it in ipairs(serpis.sidebar.items()) do
    if it.buffer == id then return it end
  end
end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local s = list[i]
    if not s then return T.finish() end
    local wait = s() or 300
    serpis.defer(wait, go)
  end
  go()
end

local events = {}
serpis.hook.add("buffer-activity-changed", function(e) table.insert(events, e) end)

local chat, wt_chat, wt_path, term
steps({
  function()
    T.check("visible by default", serpis.sidebar.visible(), true)
    T.press("s-b")
  end,
  function()
    T.check("s-b hides", serpis.sidebar.visible(), false)
    T.press("s-b")
  end,
  function()
    T.check("s-b shows", serpis.sidebar.visible(), true)
    serpis.command.run("new-chat", proj)
    chat = serpis.buffer.current()
    local it = item(chat)
    T.check("chat in project", it and it.project, proj)
    T.check("chat on branch", it and it.branch, "main")
    T.check("chat is an agent", it and it.agent, true)
    T.check("chat idle", it and it.state, "idle")
    serpis.project.new_worktree(proj, "feat/sidebar", { on_done = function(p) wt_path = p end })
    return 2500
  end,
  function()
    T.check("worktree created", wt_path ~= nil, true)
    wt_chat = serpis.buffer.current()
    local it = item(wt_chat)
    T.check("worktree chat is new", wt_chat ~= chat, true)
    T.check("worktree chat project", it and it.project, proj)
    T.check("worktree chat worktree", it and it.worktree, wt_path)
    T.check("worktree chat branch", it and it.branch, "feat/sidebar")
    serpis.command.run("sidebar-refresh")
    return 1500
  end,
  function()
    local found
    for _, p in ipairs(serpis.project.list()) do
      if p.root == proj then found = p end
    end
    T.check("project listed", found ~= nil, true)
    T.check("two worktrees", found and #found.worktrees, 2)
    T.check("main first", found and found.worktrees[1].branch, "main")
    T.check("chat grouped in worktree", found and found.worktrees[2].buffers[1], wt_chat)
    if shots then d.screenshot(shots .. "/sidebar-1.png") end
    -- Search with real keys: focus, type, pick.
    serpis.buffer.show(chat)
    T.press("s-e")
  end,
  function()
    T.check("s-e focuses", serpis.sidebar.focused(), true)
    T.press("f e a t")
  end,
  function()
    T.check("typed query", serpis.sidebar.query(), "feat")
    local sel = serpis.sidebar.selected()
    T.check("top match is worktree chat", sel and sel.id, wt_chat)
    if shots then d.screenshot(shots .. "/sidebar-search.png") end
    T.press("RET")
  end,
  function()
    T.check("RET opens", serpis.buffer.current(), wt_chat)
    T.check("RET leaves sidebar", serpis.sidebar.focused(), false)
    -- ESC clears, then leaves.
    T.press("s-e")
  end,
  function()
    T.press("x y")
  end,
  function()
    T.press("ESC")
  end,
  function()
    T.check("ESC clears query", serpis.sidebar.query(), "")
    T.check("ESC keeps focus", serpis.sidebar.focused(), true)
    T.press("ESC")
  end,
  function()
    T.check("second ESC leaves", serpis.sidebar.focused(), false)
    -- Attention: a hidden buffer that stops working.
    serpis.buffer.set_activity(chat, { state = "working", detail = "bash cargo test" })
  end,
  function()
    T.check("override working", item(chat).state, "working")
    T.check("override detail", item(chat).detail, "bash cargo test")
    serpis.buffer.set_activity(chat, nil)
  end,
  function()
    local it = item(chat)
    T.check("back to idle", it.state, "idle")
    T.check("hidden finish wants attention", it.attention, true)
    local last = events[#events]
    T.check("hook state", last and last.state, "idle")
    T.check("hook previous", last and last.previous, "working")
    serpis.sidebar.open(chat)
  end,
  function()
    T.check("looking clears attention", item(chat).attention, false)
    -- Mouse: click the worktree chat's row (rows clip to the list, so take its center).
    for _, r in ipairs(d.sidebar_rows()) do
      if r.kind == "buffer" and r.id == wt_chat then
        local x, y = r.x + r.w / 3, r.y + r.h / 2
        d.mouse_move(x, y)
        d.mouse_button(x, y, "left", true)
        d.mouse_button(x, y, "left", false)
      end
    end
  end,
  function()
    T.check("click opens row", serpis.buffer.current(), wt_chat)
    -- Clicking a project header folds it: its rows disappear.
    for _, r in ipairs(d.sidebar_rows()) do
      if r.kind == "project" and r.path == proj then
        d.mouse_move(r.x + 40, r.y + r.h / 2)
        d.mouse_button(r.x + 40, r.y + r.h / 2, "left", true)
        d.mouse_button(r.x + 40, r.y + r.h / 2, "left", false)
      end
    end
  end,
  function()
    local shown = false
    for _, r in ipairs(d.sidebar_rows()) do
      if r.kind == "buffer" and r.id == chat then shown = true end
    end
    T.check("click folds project", shown, false)
    for _, r in ipairs(d.sidebar_rows()) do
      if r.kind == "project" and r.path == proj then
        d.mouse_button(r.x + 40, r.y + r.h / 2, "left", true)
        d.mouse_button(r.x + 40, r.y + r.h / 2, "left", false)
      end
    end
  end,
  function()
    local shown = false
    for _, r in ipairs(d.sidebar_rows()) do
      if r.kind == "buffer" and r.id == chat then shown = true end
    end
    T.check("click unfolds project", shown, true)
    serpis.sidebar.open(chat)
    -- A terminal running a command works, then finishes out of sight.
    serpis.command.run("new-terminal", proj)
    term = serpis.buffer.current()
    T.check("terminal in project", item(term).project, proj)
    return 800
  end,
  function()
    d.text("sleep 2")
    T.press("RET")
    return 900
  end,
  function()
    local it = item(term)
    T.check("terminal working", it.state, "working")
    T.check("terminal program", it.detail, "sleep")
    T.check("sleep is no agent", it.agent, false)
    if shots then d.screenshot(shots .. "/sidebar-working.png") end
    serpis.buffer.show(chat)
    return 2500
  end,
  function()
    local it = item(term)
    T.check("terminal idle again", it.state, "idle")
    T.check("terminal wants attention", it.attention, true)
    -- An agent program waits for you when quiet.
    serpis.var.set("agent-programs", { "cat" })
    serpis.buffer.show(term)
    return 300
  end,
  function()
    d.text("cat")
    T.press("RET")
    return 2600
  end,
  function()
    local it = item(term)
    T.check("cat counts as agent", it.agent, true)
    T.check("quiet agent waits", it.state, "waiting")
    if shots then d.screenshot(shots .. "/sidebar-waiting.png") end
    T.press("C-c")
    T.press("C-c")
    return 500
  end,
  function()
    -- Removing the worktree kills its buffers (answer y).
    serpis.command.run("worktree-remove", wt_path)
    return 300
  end,
  function()
    T.press("y")
    return 800
  end,
  function()
    T.check("worktree buffer killed", serpis.buffer.exists(wt_chat), false)
    T.check("worktree dir gone", sh("test -d '" .. wt_path .. "' && echo yes || echo no"), "no")
    sh("rm -rf '" .. tmp .. "'")
  end,
})
