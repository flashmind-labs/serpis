-- The checklist extension (examples/extensions/checklist.lua) installed as a user would and used
-- with real keys, against the local mock model (tests/ui/mock_llm.py).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
local out = os.getenv("SERPIS_TEST_OUT")
local tmp = out .. ".d"
local plain, repo = tmp .. "/plain", tmp .. "/repo"
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. plain .. "' '" .. repo .. "/.serpis'")
sh("cd '" .. repo .. "' && git init -q && git config user.email t@t && git config user.name t"
  .. " && echo one > a.txt && git add a.txt && git commit -qm one && echo two > a.txt")
local function write(path, text)
  local f = io.open(path, "w")
  f:write(text)
  f:close()
end
write(repo .. "/.serpis/checklist.md", "intro line\n## First\n- [ ] Repo item\n- [x] Starts ticked in the file\n- [ ] Repo item\nnot an item\n")

local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
write(agent_dir .. "/models.json", string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})

local log = os.getenv("SERPIS_TEST_MOCK_LOG")
local function mock_saw(text)
  local fh = io.open(log)
  if not fh then return false end
  local all = fh:read("*a")
  fh:close()
  return all:find(text, 1, true) ~= nil
end
local function has_message(text)
  for _, m in ipairs(d.messages()) do
    if m:find(text, 1, true) then return true end
  end
  return false
end
local function labels()
  local list = {}
  for _, c in ipairs(d.candidates() or {}) do table.insert(list, c.label) end
  return list
end
local function settled(buf, f, tries)
  tries = tries or 0
  if serpis.chat.running(buf) and tries < 100 then
    return serpis.defer(100, function() settled(buf, f, tries + 1) end)
  end
  serpis.defer(300, f)
end

local i = 0
local list
local function go()
  i = i + 1
  local s = list[i]
  if not s then return T.finish() end
  local wait = s(go)
  if wait ~= false then serpis.defer(wait or 300, go) end
end

local chat, repo_chat
local ticked = {}
local ask = "Did I miss anything?"
local ASK_ITEM = "Ask: " .. ask
local function done_count()
  local n = 0
  for _, c in ipairs(d.candidates() or {}) do
    if c.checked then n = n + 1 end
  end
  return n
end
local function click(rect)
  local x, y = rect.x + rect.w / 2, rect.y + rect.h / 2
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
end
list = {
  function()
    local ext = serpis.config_dir .. "/extensions"
    sh("mkdir -p '" .. ext .. "' && cp '" .. os.getenv("SERPIS_TEST_DIR") .. "'/../../examples/extensions/checklist.lua '" .. ext .. "'")
    serpis.reload()
    serpis.hook.add("checklist-ticked", function(e) table.insert(ticked, e) end)
    T.room()
    serpis.command.run("new-chat", plain)
  end,
  function()
    chat = serpis.buffer.current()
    T.check("nothing opens by itself on a new chat", d.minibuffer(), nil)
    T.press("C-c x")
  end,
  function()
    T.check("C-c x opens the list", d.minibuffer(), "Checklist")
    local c = d.candidates()
    T.check("with the default's 8 items", #c, 8)
    T.check("in the file's order, as a checkbox", c[1].label .. "/" .. tostring(c[1].checked), "I wrote down what I expect the answer to be/false")
    T.check("under its section", c[1].section, "Before starting")
    T.check("Ask: is left out of the row", c[7].label, ask)
    T.check("which says what it does", c[7].detail, "asks the agent")
    T.press("SPC")
  end,
  function()
    T.check("SPC ticks once, and the list stays open", d.minibuffer(), "Checklist")
    T.check("the tick shows", d.candidates()[1].checked, true)
    T.check("one tick", done_count(), 1)
    T.check("the hook heard it", ticked[1] and ticked[1].done, true)
    T.check("the hook names the chat", ticked[1] and ticked[1].buffer, chat)
    T.press("j")
  end,
  function()
    T.press("RET")
  end,
  function()
    T.check("j moves on and RET ticks the next, keeping the place", d.candidates()[2].checked, true)
    d.screenshot(out .. ".pill.png")
    T.press("k")
  end,
  function()
    T.press("x")
  end,
  function()
    T.check("other typing does nothing", done_count(), 2)
    click(d.rect("palette-selected-row"))
  end,
  function()
    T.check("a click on a row unticks it", d.candidates()[1].checked, false)
    T.check("the hook heard that", ticked[3] and ticked[3].done, false)
    T.press("ESC")
  end,
  function()
    T.check("ESC leaves", d.minibuffer(), nil)
    click(d.rect("header-status-button"))
  end,
  function()
    T.check("a click on the pill opens it", d.minibuffer(), "Checklist")
    T.press("j j j j j j") -- down to "Did I miss anything?"
  end,
  function()
    T.press("RET")
  end,
  function()
    T.check("an Ask item closes the list", d.minibuffer(), nil)
    local e = serpis.chat.entries(chat, "checklist")
    T.check("and is ticked", e[#e].item, ASK_ITEM)
    settled(chat, go)
    return false
  end,
  function()
    T.check("the model got the question, without Ask:", mock_saw('"text": "' .. ask .. '"'), true)
    T.check("Ask: itself never reached the model", mock_saw("Ask: "), false)
    T.check("the list never reached the model", mock_saw("I wrote down what I expect"), false)
    T.press("C-c x")
  end,
  function()
    T.check("progress counts it", done_count(), 2)
    T.press("j j j j j j")
  end,
  function()
    T.press("RET")
  end,
  function()
    T.check("picked again it only unticks", d.candidates()[7].checked, false)
    T.check("and stays open", d.minibuffer(), "Checklist")
    T.press("ESC")
  end,
  function()
    serpis.command.run("chat-checklist", ASK_ITEM)
    serpis.chat.prompt(chat, "SHELL:sleep 2")
  end,
  function()
    T.check("the agent is working", serpis.chat.running(chat), true)
    serpis.command.run("chat-checklist", "Ask: List the decisions and assumptions you made that I didn't specify.")
    T.check("an Ask item waits for the turn to end", has_message("ask once its turn ends"), true)
    local last = serpis.chat.entries(chat, "checklist")
    T.check("and isn't ticked", last[#last].item, ASK_ITEM)
    settled(chat, go)
    return false
  end,
  function()
    -- Ticks live in the session: resume it and they hold.
    local path = serpis.session.of(chat)
    T.check("session saved", path ~= nil, true)
    serpis.buffer.kill(chat)
    chat = serpis.session.resume(path)
  end,
  function()
    serpis.buffer.show(chat)
    serpis.command.run("chat-checklist")
  end,
  function()
    T.check("ticks survive resuming", done_count(), 2)
    T.press("ESC")
  end,
  function()
    serpis.command.run("chat-checklist-reset")
    serpis.command.run("chat-checklist")
  end,
  function()
    T.check("reset unticks everything", done_count(), 0)
    T.press("ESC")
  end,
  function()
    -- A repository's own list replaces the default.
    serpis.command.run("new-chat", repo)
  end,
  function()
    repo_chat = serpis.buffer.current()
    T.press("C-c x")
  end,
  function()
    local c = d.candidates()
    T.check("a repository's list is used, a repeated item once", #c, 2)
    T.check("ticks in the file don't count", c[2].label .. "/" .. tostring(c[2].checked), "Starts ticked in the file/false")
    T.check("its section", c[1].section, "First")
    T.press("ESC")
    serpis.chat.prompt(repo_chat, "hello")
    settled(repo_chat, go)
    return false
  end,
  function()
    T.press("s-D")
    return 800
  end,
  function()
    T.check("the diff page is open", serpis.buffer.kind(), "diff")
    T.press("X")
  end,
  function()
    T.check("X on the diff page opens the chat's list", #(d.candidates() or {}), 2)
    T.press("RET")
  end,
  function()
    T.check("and ticks in that chat", serpis.chat.entries(repo_chat, "checklist")[1].item, "Repo item")
    T.press("ESC")
  end,
  function()
    -- A list with nothing in it: no pill, and no empty picker.
    write(repo .. "/.serpis/checklist.md", "nothing here\n")
    serpis.buffer.show(repo_chat)
  end,
  function()
    serpis.command.run("chat-checklist")
  end,
  function()
    T.check("an empty list opens nothing", d.minibuffer(), nil)
    T.check("and says so", has_message("The checklist has no items"), true)
    -- checklist-file is honored in a folder with no list of its own.
    write(tmp .. "/mine.md", "- [ ] Mine\n")
    serpis.var.set("checklist-file", tmp .. "/mine.md")
    serpis.buffer.show(chat)
    serpis.command.run("chat-checklist")
  end,
  function()
    T.check("checklist-file picks the list", #(d.candidates() or {}), 1)
    T.check("a list with no sections", d.candidates()[1].section, nil)
    T.press("ESC")
  end,
  function()
    -- An ordinary prompt keeps its search field and its typing.
    serpis.command.run("switch-to-buffer")
  end,
  function()
    d.text("zz")
  end,
  function()
    T.check("other prompts still take typing", d.minibuffer_input(), "zz")
    T.press("ESC")
    T.check("loading it made no errors", has_message("error"), false)
  end,
}
go()
