-- A bash tool call's card: the command highlighted as shell (the command it runs, its flags,
-- quotes, $variables and operators each in a color), its later lines too, then what it printed.
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
local function has(text) return (serpis.chat.transcript() or ""):find(text, 1, true) ~= nil end
-- A row of the transcript, whole (the prompt shows the same words, run together).
local function row(text) return ("\n" .. (serpis.chat.transcript() or "") .. "\n"):find("\n" .. text .. "\n", 1, true) ~= nil end

local command = table.concat({
  [[NAME="serpis tests" && echo "hello, $NAME" | grep -E 'hello|bye' --color=never 2>&1 # say it]],
  [[for n in 1 2; do]],
  [[  printf '%s\n' "line ${n}" > /dev/null]],
  [[done; echo $(echo nested) ok]],
}, "\n")

-- With SERPIS_TEST_SHOTS, a screenshot before the next step. Ends a step that waits (false).
local function shot(name, go)
  local shots = os.getenv("SERPIS_TEST_SHOTS")
  if not shots then serpis.defer(100, go); return false end
  serpis.defer(800, function() d.screenshot(shots .. "/" .. name); serpis.defer(500, go) end)
  return false
end

local chat, chain
local steps = {
  function(go)
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "SHELL:" .. command)
    when(function() return has("nested ok") and not serpis.chat.running(chat) end, go)
    return false
  end,
  function(go)
    return shot("bash-highlight.png", go)
  end,
  function(go)
    -- A long chain: each step on its row, the operators kept, and what it printed below.
    serpis.command.run("new-chat")
    chain = serpis.buffer.current()
    serpis.chat.prompt(chain, "SHELL:" .. "cd /tmp && echo 'not && split' | tr a-z A-Z 2>&1 || echo failed; echo last-step")
    when(function() return has("NOT && SPLIT") and not serpis.chat.running(chain) end, go)
    return false
  end,
  function(go)
    return shot("bash-steps.png", go)
  end,
  function(go)
    T.check("a chain's first step has its row", row("cd /tmp &&"), true)
    T.check("and the next, its pipe and quotes whole", row("echo 'not && split' | tr a-z A-Z 2>&1 ||"), true)
    T.check("and the next", row("echo failed ;"), true)
    T.check("and the last", row("echo last-step"), true)
    T.check("the header counts them", has("4 steps"), true)
    T.check("what it printed follows", has("NOT && SPLIT"), true)
  end,
  function(go)
    -- A long chain printing a lot: the steps stay in view, the first six, above the output's tail.
    serpis.command.run("new-chat")
    chain = serpis.buffer.current()
    serpis.chat.prompt(chain, "SHELL:cd /tmp && echo step-two && echo step-three && echo step-four && echo step-five && "
      .. "echo a-long-step-that-has-to-wrap --with-many --flags-and-arguments --that-go-on --for-longer --than-the-card-is-wide --so-it-wraps-clear-of-the-gutter && seq 1 30 && echo step-eight")
    when(function() return has("step-eight") and not serpis.chat.running(chain) end, go)
    return false
  end,
  function(go)
    T.check("a closed card keeps its first steps", has("cd /tmp &&"), true)
    T.check("up to the sixth", has("--so-it-wraps-clear-of-the-gutter &&"), true)
    T.check("and says what it left out", has("2 more steps"), true)
    T.check("a later step waits", row("seq 1 30 &&"), false)
    T.check("the tail of what it printed shows too", has("30"), true)
    return shot("bash-steps-long.png", go)
  end,
  function(go)
    -- Clicked, the card opens.
    serpis.command.run("chat-focus-transcript")
    local n = 1
    for l in (serpis.chat.transcript() .. "\n"):gmatch("(.-)\n") do
      if l:find("2 more steps", 1, true) then break end
      n = n + 1
    end
    serpis.defer(800, function()
      local r = d.cell_rect(n, 0)
      d.mouse_move(r.x + 4, r.y + 4)
      d.mouse_button(r.x + 4, r.y + 4, "left", true)
      d.mouse_button(r.x + 4, r.y + 4, "left", false)
      serpis.defer(400, go)
    end)
    return false
  end,
  function(go)
    T.check("opened, the card shows them all", row("seq 1 30 &&") and row("echo step-eight"), true)
    T.check("and nothing says more", has("more steps"), false)
    return shot("bash-steps-open.png", go)
  end,
  function(go)
    -- Turned off, a chain is one command again (once the transcript next redraws).
    serpis.var.set("chat-shell-steps", false)
    serpis.command.run("chat-toggle-fold")
    serpis.defer(300, go)
    return false
  end,
  function()
    T.check("the setting turns it off", row("seq 1 30 &&"), false)
    T.check("and the header doesn't count", has("8 steps"), false)
    serpis.var.set("chat-shell-steps", true)
  end,
  function()
    local has = function(text) return serpis.chat.transcript(chat):find(text, 1, true) ~= nil end
    T.check("the card shows the command's first line", has([[echo "hello, $NAME" | grep -E 'hello|bye']]), true)
    T.check("and its later lines", has([[printf '%s\n' "line ${n}" > /dev/null]]), true)
    T.check("and what it printed", has("hello, serpis tests"), true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(400, go) end
end
go()
