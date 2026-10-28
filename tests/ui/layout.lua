local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
serpis.keys.scheme("vim")

-- Compact layout shape: H[a V[b c]] with buffer names.
local function shape()
  local function node(n)
    if n.buffer then return serpis.buffer.name(n.buffer) end
    local k = ({ splith = "H", splitv = "V", tabbed = "T", stacked = "S" })[n.layout]
    local parts = {}
    for _, c in ipairs(n.children) do table.insert(parts, node(c)) end
    return k .. "[" .. table.concat(parts, " ") .. "]"
  end
  return node(serpis.window.configuration().tree)
end
local function cur() return serpis.buffer.name(serpis.buffer.current()) end
local function new(name)
  local id = serpis.buffer.create("text", name, { text = name })
  serpis.buffer.show(id)
  return id
end

-- Start from one window on a.
serpis.window.only()
new("a")
serpis.window.split("right"); new("b")
serpis.window.split("right"); new("c")
T.check("splits append in one container", shape(), "H[a b c]")
serpis.window.split("below"); new("d")
T.check("perpendicular split wraps", shape(), "H[a b V[c d]]")

-- i3 focus follows the tree.
serpis.window.focus("left")
T.check("focus left", cur(), "b")
serpis.window.focus("right")
T.check("focus right re-enters last focused", cur(), "d")

-- i3 move: swap, enter, leave, edge.
serpis.window.focus("left")
serpis.window.move("right")
T.check("move into container", shape(), "H[a V[c d b]]")
serpis.window.move("left")
T.check("move out of container", shape(), "H[a b V[c d]]")
serpis.window.move("left")
T.check("move swaps with window", shape(), "H[b a V[c d]]")
T.check("moved window keeps focus", cur(), "b")
serpis.command.run("window-move-up")
T.check("edge move re-lays the root", shape(), "V[b H[a V[c d]]]")
serpis.command.run("window-move-down")
T.check("moving down enters the container below", shape(), "H[a b V[c d]]")

-- Layout kinds on the focused window's container.
serpis.window.focus("left")
T.check("focus a", cur(), "a")
serpis.window.layout("tabbed")
T.check("tabbed", shape(), "T[a b V[c d]]")
serpis.window.focus("right")
T.check("tabs reached with right", cur(), "b")
serpis.command.run("layout-toggle-all")
T.check("cycle tabbed -> splith", serpis.window.layout(), "splith")
serpis.command.run("layout-stacked")
T.check("stacked", shape(), "S[a b V[c d]]")
serpis.command.run("layout-splith")

-- focus-parent selects a container; window commands then act on all of it.
serpis.window.focus("right")
T.check("into the column", cur(), "d")
serpis.command.run("focus-parent")
serpis.command.run("window-move-left")
T.check("container moves as a unit", shape(), "H[a V[c d] b]")
serpis.command.run("focus-child")
serpis.window.move("right")
T.check("focus-child: window moves alone again", shape(), "H[a c d b]")

-- Fullscreen shows the focused window alone.
serpis.command.run("window-fullscreen-toggle")
T.check("fullscreen", serpis.window.configuration().fullscreen, true)
serpis.command.run("window-fullscreen-toggle")
T.check("fullscreen off", serpis.window.configuration().fullscreen, false)

-- Closing a window never kills its buffer.
local closing = serpis.buffer.current()
local closing_name = cur()
serpis.command.run("close-window")
T.check("closed view", serpis.window.count(), 3)
T.check("buffer survives", serpis.buffer.exists(closing), true)
local hidden = {}
for _, id in ipairs(serpis.buffer.hidden()) do hidden[serpis.buffer.name(id)] = true end
T.check("closed buffer is hidden", hidden[closing_name], true)

-- Workspaces in core: switch, back and forth, move a window.
local ws1 = shape()
T.check("starts on 1", serpis.workspace.current(), "1")
serpis.workspace.switch(2)
T.check("switch to 2", serpis.workspace.current(), "2")
T.check("a new workspace is empty", shape(), "H[*empty*]")
serpis.command.run("workspace-back-and-forth")
T.check("back and forth", serpis.workspace.current(), "1")
T.check("layout kept", shape(), ws1)

-- As in i3: what opens in an empty workspace fills it; closing a window closes it (nothing
-- duplicated, nothing swapped in), the last leaves the workspace empty; killing a buffer closes
-- its windows, in any workspace.
serpis.workspace.switch(8)
local x = new("x")
T.check("what opens fills an empty workspace", shape(), "H[x]")
serpis.window.split("right"); local y = new("y")
serpis.command.run("close-window")
T.check("closing a window closes it", shape(), "H[x]")
serpis.command.run("close-window")
T.check("closing the last leaves the workspace empty", shape(), "H[*empty*]")
T.check("the empty page is no buffer of yours", (function()
  for _, b in ipairs(serpis.buffer.list()) do if b.kind == "empty" then return false end end
  return true
end)(), true)
serpis.buffer.show(x)
serpis.window.split("right"); serpis.buffer.show(y)
serpis.buffer.kill(y)
T.check("killing a buffer closes its window", shape(), "H[x]")
serpis.buffer.kill(x)
T.check("and the last leaves the workspace empty", shape(), "H[*empty*]")
local z, w = new("z"), nil
serpis.window.split("right"); w = new("w")
serpis.workspace.switch(9)
serpis.buffer.kill(w)
serpis.workspace.switch(8)
T.check("a buffer killed elsewhere closes its window here too", shape(), "H[z]")
serpis.var.set("kill-buffer-closes-window", false)
serpis.window.split("right"); local v = new("v")
serpis.buffer.kill(v)
T.check("kill-buffer-closes-window off: its window shows what it showed before", shape(), "H[z z]")
serpis.var.set("kill-buffer-closes-window", true)
serpis.buffer.kill(z)
serpis.workspace.switch(1)
serpis.window.focus("left"); serpis.window.focus("left"); serpis.window.focus("left")
T.check("focus a again", cur(), "a")
serpis.workspace.move_window(3)
T.check("moved out of 1", serpis.window.count(), 2)
serpis.workspace.switch(3)
T.check("arrived on 3", shape(), "H[a]")
local names = {}
for _, w in ipairs(serpis.workspace.list()) do table.insert(names, w.name) end
T.check("2 was dropped when empty", table.concat(names, ","), "1,3")
serpis.var.set("workspace-display-names", { ["3"] = "三" })
T.check("display names", serpis.workspace.list()[2].display, "三")
serpis.workspace.switch(1)

-- Call `f` once `ready()` holds (polled every 100ms, up to 15s).
local function when(ready, f, tries)
  tries = tries or 150
  if ready() or tries == 0 then return f() end
  serpis.defer(100, function() when(ready, f, tries - 1) end)
end
local function prompt_ready() return serpis.buffer.text():find("%S") ~= nil end
local function running(id) return function() local p = serpis.buffer.process(id); return p and p.running end end

-- Killing: an idle shell goes at once; a running program asks first. Quitting asks only about
-- terminals that end with the window: one a terminal node holds keeps running.
serpis.command.run("new-terminal")
local term = serpis.buffer.current()
when(prompt_ready, function() serpis.defer(700, function()
  T.check("idle shell", serpis.buffer.process(term).running, false)
  serpis.buffer.insert("sleep 30\r")
  when(running(term), function()
    local p = serpis.buffer.process(term)
    T.check("running program detected", p.running and p.name, "sleep")
    serpis.command.run("kill-current-buffer")
    T.check("kill asks first", serpis.buffer.exists(term), true)
    serpis.keys.feed("n")
    T.check("n keeps it", serpis.buffer.exists(term), true)
    serpis.command.run("quit")
    serpis.keys.feed("n")
    T.check("quit asks too", serpis.buffer.exists(term), true)
    serpis.command.run("kill-current-buffer")
    serpis.keys.feed("y")
    T.check("y kills it", serpis.buffer.exists(term), false)
    -- The setting turns the question off (here for a terminal its node holds).
    serpis.var.set("terminal-persistent", true)
    serpis.var.set("confirm-kill-processes", "never")
    serpis.command.run("new-terminal")
    local t2 = serpis.buffer.current()
    when(prompt_ready, function() serpis.defer(700, function()
      serpis.buffer.insert("sleep 30\r")
      when(running(t2), function()
        serpis.command.run("kill-current-buffer")
        T.check("never: killed without asking", serpis.buffer.exists(t2), false)
        T.finish()
      end)
    end) end)
  end)
end) end)
