-- The buffer switcher previews the selected buffer, in each layout minibuffer-preview offers
-- (right, down, window; bottom minibuffer), with real keys; previewing changes nothing.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
serpis.var.set("minibuffer-preview-delay", 0)
local shots = os.getenv("SERPIS_TEST_SHOTS")

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    local wait = list[i]()
    if wait ~= false then serpis.defer(wait or 300, go) end
  end
  _G.next_step = go
  go()
end
-- With SERPIS_TEST_SHOTS, a screenshot as a step of its own (it is taken a frame or two later,
-- so nothing may change before the next step).
local function shot(name)
  return function()
    if not shots then return 0 end
    d.screenshot(shots .. "/" .. name .. ".png")
    return 700
  end
end
-- Press C-n until the preview shows buffer `id` (then `f`), up to 10 times.
local function select(id, f, tries)
  tries = tries or 10
  local p = d.preview()
  if (p and p.buffer == id) or tries == 0 then return f() end
  T.press("C-n")
  serpis.defer(150, function() select(id, f, tries - 1) end)
end

local lines = {}
for i = 1, 200 do lines[#lines + 1] = "note line " .. i end
local notes, term, chat, start, windows_before
steps({
  function()
    notes = serpis.buffer.create("text", "*preview-notes*", { text = table.concat(lines, "\n") })
    serpis.command.run("new-terminal")
    term = serpis.buffer.current()
    return 1500
  end,
  function()
    -- A resize reaches the shell as SIGWINCH: it says so in the terminal.
    serpis.buffer.insert("TRAPWINCH() { echo RESIZED-BY-SOMEONE }; clear; echo PREVIEW-MARK\r")
    return 800
  end,
  function()
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "hello from the preview test")
    return 2500
  end,
  function()
    start = serpis.buffer.create("text", "*preview-start*", { text = "where we start" })
    serpis.buffer.show(start)
    windows_before = table.concat(serpis.window.buffers(), ",")
    serpis.var.set("minibuffer-preview", "right")
    d.key("P", { "cmd" })
    return 400
  end,
  function()
    T.check("switcher open", d.minibuffer(), "Switch to buffer: ")
    local p = d.preview()
    T.check("right: something previewed at once", p ~= nil and p.layout, "right")
    select(term, next_step)
    return false
  end,
  function()
    local p = d.preview()
    T.check("right: the terminal", p and p.buffer, term)
    local pane = d.rect("minibuffer-preview")
    T.check("right: pane drawn", pane ~= nil and pane.w > 100 and pane.h > 100, true)
    local row = d.rect("palette-selected-row")
    T.check("right: pane beside the list", pane ~= nil and row ~= nil and pane.x >= row.x + row.w - 1, true)
    return 0
  end,
  shot("right-terminal"),
  function()
    select(chat, next_step)
    return false
  end,
  function()
    T.check("right: the chat", (d.preview() or {}).buffer, chat)
    return 300
  end,
  shot("right-chat"),
  function()
    select(notes, next_step)
    return false
  end,
  function()
    T.check("right: the text buffer", (d.preview() or {}).buffer, notes)
    -- S-<down> scrolls the preview, not the buffer's window view.
    T.press("S-<down> S-<down>")
    return 300
  end,
  function()
    T.check("scrolling keeps the switcher", d.minibuffer(), "Switch to buffer: ")
    return 0
  end,
  shot("right-text-scrolled"),
  function()
    -- C-/ hides it, and shows it again.
    T.press("C-/")
    return 250
  end,
  function()
    T.check("C-/ hides", d.preview(), nil)
    T.check("hidden: no pane", d.rect("minibuffer-preview"), nil)
    T.press("C-/")
    return 250
  end,
  function()
    T.check("C-/ shows again", (d.preview() or {}).layout, "right")
    -- M-/ moves it: right → down → window → off.
    T.press("M-/")
    return 300
  end,
  function()
    T.check("M-/: down", (d.preview() or {}).layout, "down")
    local pane, row = d.rect("minibuffer-preview"), d.rect("palette-selected-row")
    T.check("down: pane under the list", pane ~= nil and row ~= nil and pane.y >= row.y + row.h - 1, true)
    return 0
  end,
  shot("down-text"),
  function()
    T.press("M-/")
    return 300
  end,
  function()
    T.check("M-/: window", (d.preview() or {}).layout, "window")
    T.check("window: the switcher stays open", d.minibuffer(), "Switch to buffer: ")
    return 0
  end,
  shot("window-text"),
  function()
    T.press("M-/")
    return 300
  end,
  function()
    T.check("M-/: off", d.preview(), nil)
    T.press("ESC")
    return 300
  end,
  function()
    T.check("ESC: back where we started", serpis.buffer.current(), start)
    T.check("ESC: windows unchanged", table.concat(serpis.window.buffers(), ","), windows_before)
    -- minibuffer-preview "window": the focused window shows it, at full size.
    serpis.var.set("minibuffer-preview", "window")
    d.key("P", { "cmd" })
    return 400
  end,
  function()
    select(term, next_step)
    return false
  end,
  function()
    local p = d.preview()
    T.check("window: the terminal", p and p.layout == "window" and p.buffer, term)
    T.check("window: drawn in the window", d.rect("minibuffer-preview") ~= nil, true)
    T.check("window: not switched", serpis.buffer.current(), start)
    return 0
  end,
  shot("window-terminal"),
  function()
    T.press("ESC")
    return 300
  end,
  function()
    T.check("window ESC: back where we started", serpis.buffer.current(), start)
    T.check("window ESC: windows unchanged", table.concat(serpis.window.buffers(), ","), windows_before)
    -- The bottom minibuffer: "auto" previews in the window (vertico + consult).
    serpis.var.set("minibuffer-style", "bottom")
    serpis.var.set("minibuffer-preview", "auto")
    d.key("P", { "cmd" })
    return 400
  end,
  function()
    select(chat, next_step)
    return false
  end,
  function()
    local p = d.preview()
    T.check("bottom auto: window", p and p.layout, "window")
    T.check("bottom auto: the chat", p and p.buffer, chat)
    return 0
  end,
  shot("bottom-window-chat"),
  function()
    -- And beside the list when asked.
    serpis.var.set("minibuffer-preview", "right")
    return 300
  end,
  function()
    T.check("bottom right", (d.preview() or {}).layout, "right")
    local pane = d.rect("minibuffer-preview")
    T.check("bottom right: pane drawn", pane ~= nil and pane.w > 100, true)
    return 0
  end,
  shot("bottom-right-chat"),
  function()
    -- RET switches to the previewed buffer, as without a preview.
    T.press("RET")
    return 400
  end,
  function()
    T.check("RET switches", serpis.buffer.current(), chat)
    serpis.var.set("minibuffer-style", "palette")
    serpis.buffer.show(start)
    -- "auto" in the palette: beside the list when there is room, else under it.
    serpis.var.set("minibuffer-preview", "auto")
    d.key("P", { "cmd" })
    return 400
  end,
  function()
    local l = (d.preview() or {}).layout
    T.check("auto: right or down", l == "right" or l == "down", true)
    if T.narrow then T.check("auto narrow: down", l, "down") end
    T.press("ESC")
    return 300
  end,
  function()
    -- Previewing never resized the terminal (its shell would have said so). Read in the same
    -- tick it is shown, before a frame gives it the window's size.
    serpis.buffer.show(term)
    local text = serpis.buffer.text() or ""
    serpis.buffer.show(start)
    T.check("terminal never resized", text:find("RESIZED-BY-SOMEONE", 1, true), nil)
    T.check("terminal shows its output", text:find("PREVIEW-MARK", 1, true) ~= nil, true)
    -- Lua pickers preview with `preview`.
    serpis.var.set("minibuffer-preview", "right")
    serpis.minibuffer.read({
      prompt = "Pick: ",
      candidates = { "alpha", "beta" },
      sort = false,
      preview = function(v) return { title = "about " .. v, lines = { v .. " 1", v .. " 2", v .. " 3" } } end,
      on_accept = function() end,
    })
    return 300
  end,
  function()
    local p = d.preview() or {}
    T.check("lua: text preview", p.title, "about alpha")
    T.check("lua: its lines", p.lines, 3)
    T.press("C-n")
    return 300
  end,
  function()
    T.check("lua: follows the selection", (d.preview() or {}).title, "about beta")
    return 0
  end,
  shot("lua-text"),
  function()
    T.press("ESC")
    return 300
  end,
  function()
    serpis.minibuffer.read({
      prompt = "Pick: ",
      candidates = { { label = "the chat", value = tostring(chat) } },
      preview_layout = "down",
      preview = function(v) return { buffer = tonumber(v) } end,
      on_accept = function() end,
    })
    return 300
  end,
  function()
    local p = d.preview() or {}
    T.check("lua: a buffer", p.buffer, chat)
    T.check("lua: its own layout", p.layout, "down")
    T.press("ESC")
    return 300
  end,
  function()
    -- A prompt without previews shows none (M-x).
    serpis.command.run("execute-extended-command")
    return 300
  end,
  function()
    T.check("no preview for commands", d.preview(), nil)
    T.press("ESC")
    return 200
  end,
})
