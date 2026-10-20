-- Buffers show the same names everywhere, and folders can be picked interactively.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("session-directories", {})
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
os.execute("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "/alpha' '" .. tmp .. "/beta' && cd '" .. tmp .. "/alpha' && git init -q")
local p = io.popen("cd '" .. tmp .. "' && pwd -P"); local root = p:read("*l"); p:close()

local function steps(list)
  local i = 0
  local function go() i = i + 1; if not list[i] then os.execute("rm -rf '" .. tmp .. "'"); return T.finish() end; serpis.defer(list[i]() or 400, go) end
  go()
end
local function labels()
  local out = {}
  for _, c in ipairs(d.candidates()) do out[c.label] = c.detail end
  return out
end

local titled, before
steps({
  function()
    serpis.command.run("new-chat", root .. "/alpha")
    titled = serpis.buffer.current()
    serpis.buffer.set_title(titled, "Fix the reconnect loop")
    serpis.command.run("new-terminal", root .. "/alpha")
    serpis.command.run("new-chat", root .. "/beta")
    -- The buffer switcher, with real keys.
    d.key("P", {"cmd"})
  end,
  function()
    local l = labels()
    T.check("switcher: titled chat", l["Fix the reconnect loop"] ~= nil, true)
    T.check("switcher: untitled chat", l["Chat 3"] ~= nil, true)
    T.check("switcher: first chat", l["Chat"] ~= nil, true)
    T.check("switcher: terminal", l["Terminal"] ~= nil, true)
    T.check("switcher: no raw names", l["*chat*<2>"] == nil and l["*terminal*"] == nil, true)
    T.check("switcher: detail tells them apart", (l["Fix the reconnect loop"] or ""):find("alpha") ~= nil, true)
    T.press("ESC")
  end,
  function()
    T.check("switcher closed", d.minibuffer(), nil)
    -- The folder picker: a typed path.
    before = serpis.buffer.current()
    serpis.command.run("new-chat-in-folder")
    return 700
  end,
  function()
    T.check("folder picker open", d.minibuffer(), "New chat in: ")
    local l = labels()
    T.check("offers Browse…", l["Browse…"] ~= nil, true)
    T.check("offers known project", l["alpha"] ~= nil, true)
    d.text(root .. "/bet")
  end,
  function()
    T.check("completes folders", labels()["beta/"] ~= nil, true)
    T.press("RET")
  end,
  function()
    T.check("a new chat", serpis.buffer.current() ~= before, true)
    T.check("chat in picked folder", serpis.buffer.directory(), root .. "/beta")
    T.check("picker closed", d.minibuffer(), nil)
    -- project-open: pick a known project by name; it gets pinned.
    d.key("O", {"cmd", "shift"})
    return 700
  end,
  function()
    T.check("⌘⇧O opens the project picker", d.minibuffer(), "Open project: ")
    before = serpis.buffer.current()
    d.text("alpha")
  end,
  function()
    T.press("RET")
  end,
  function()
    T.check("another new chat", serpis.buffer.current() ~= before, true)
    T.check("chat opened in project", serpis.buffer.directory(), root .. "/alpha")
    local pinned = false
    for _, pr in ipairs(serpis.var.get("projects") or {}) do
      if pr == root .. "/alpha" then pinned = true end
    end
    T.check("project pinned", pinned, true)
  end,
})
