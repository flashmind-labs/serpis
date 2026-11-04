-- A run of commands: each that went well closes to a line (its description, else the command),
-- opened with a click; what runs stays open with a spinner, and so does what failed.
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

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(300, go)
end
local function transcript() return serpis.chat.transcript() or "" end
local function has(text) return transcript():find(text, 1, true) ~= nil end
-- A row of the transcript, whole (the prompt shows the same words, run together).
local function row(text) return ("\n" .. transcript() .. "\n"):find("\n" .. text .. "\n", 1, true) ~= nil end
local function shot(name, go)
  local shots = os.getenv("SERPIS_TEST_SHOTS")
  if not shots then serpis.defer(100, go); return false end
  serpis.defer(800, function() d.screenshot(shots .. "/" .. name); serpis.defer(500, go) end)
  return false
end
-- Clicks the transcript's first row containing `text`.
local function click_row(text)
  local n = 1
  for l in (transcript() .. "\n"):gmatch("(.-)\n") do
    if l:find(text, 1, true) then break end
    n = n + 1
  end
  local r = d.cell_rect(n, 0)
  d.mouse_move(r.x + 4, r.y + 4)
  d.mouse_button(r.x + 4, r.y + 4, "left", true)
  d.mouse_button(r.x + 4, r.y + 4, "left", false)
end
local chat = function() return serpis.buffer.current() end

local steps = {
  function(go)
    serpis.command.run("new-chat")
    serpis.chat.prompt(chat(), "STEPS:Check the repo state|echo clean-out;;Find where cards are drawn|cd /tmp && echo found-out && echo twice-out;;Run the test suite|echo passed-out")
    when(function() return row("done") and not serpis.chat.running(chat()) end, go)
    return false
  end,
  function(go)
    T.check("a described command's line says what it is for", has("Check the repo state"), true)
    T.check("a chain's line counts its steps", has("Find where cards are drawn   3 steps"), true)
    T.check("closed, its command doesn't show", row("cd /tmp &&"), false)
    T.check("nor what it printed", row("found-out"), false)
    T.check("nor the usage of the turns on the way", select(2, transcript():gsub("↳", "")), 1)
    return shot("cards-list.png", go)
  end,
  function(go)
    serpis.command.run("chat-focus-transcript")
    serpis.defer(800, function()
      click_row("✓  Find where cards are drawn")
      serpis.defer(400, go)
    end)
    return false
  end,
  function(go)
    T.check("clicked, it opens to its steps", row("cd /tmp &&") and row("echo found-out &&") and row("echo twice-out"), true)
    T.check("and what it printed", row("found-out") and row("twice-out"), true)
    T.check("the others stay closed", row("clean-out"), false)
    return shot("cards-open.png", go)
  end,
  function(go)
    click_row("✓  Run the test suite")
    serpis.defer(400, go)
    return false
  end,
  function(go)
    T.check("a described single command opens to the command too", row("echo passed-out") and row("passed-out"), true)
    -- Not described: the command itself, and a chain's first step that isn't just a cd.
    serpis.command.run("new-chat")
    serpis.chat.prompt(chat(), "SHELL:cd /tmp && echo plain-out; echo more-out")
    when(function() return row("done") and not serpis.chat.running(chat()) end, go)
    return false
  end,
  function(go)
    T.check("no description: the first step that does something", has("bash  echo plain-out   3 steps"), true)
    T.check("and what it printed waits", row("plain-out"), false)
    serpis.command.run("new-chat")
    serpis.chat.prompt(chat(), "SHELL:echo bad-out; exit 3")
    when(function() return row("done") and not serpis.chat.running(chat()) end, go)
    return false
  end,
  function(go)
    T.check("what failed stays open", row("bad-out"), true)
    T.check("with its command", row("echo bad-out ;"), true)
  end,
  function(go)
    serpis.command.run("new-chat")
    serpis.chat.prompt(chat(), "DESCRIBED:Wait for it|echo started-out; sleep 4; echo slept-out")
    when(function() return has("Wait for it") and row("started-out") end, go)
    return false
  end,
  function(go)
    T.check("what runs stays open: its command", row("echo started-out ;"), true)
    T.check("and what it has printed", row("started-out"), true)
    return shot("cards-running.png", go)
  end,
  function(go)
    when(function() return row("done") and not serpis.chat.running(chat()) end, go)
    return false
  end,
  function(go)
    T.check("finished, it closes", row("echo started-out ;"), false)
    serpis.var.set("chat-tool-compact", false)
    serpis.command.run("new-chat")
    serpis.chat.prompt(chat(), "SHELL:echo open-out")
    when(function() return row("done") and not serpis.chat.running(chat()) end, go)
    return false
  end,
  function()
    T.check("the setting keeps cards open", row("open-out"), true)
    serpis.var.set("chat-tool-compact", true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(400, go) end
end
go()
