-- A chat follows new output while you are at its end: scrolling up lets go, scrolling back to the
-- end picks it up again.
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
  serpis.defer(200, go)
end
local chat = function() return serpis.buffer.current() end
local function rows()
  local n = 1
  for _ in (serpis.chat.transcript() or ""):gmatch("\n") do n = n + 1 end
  return n
end
local function last_seen() return select(2, serpis.buffer.visible()) end
-- Rows of output below the window (up to a constant: the transcript leaves some of the view's
-- rows out). It holds still while the view follows, and grows when it doesn't.
local function behind() return rows() - last_seen() end
-- What arrives, one row at a time, for as long as the test needs.
local function run_steps(n)
  local s = {}
  for i = 1, n do s[i] = "Step " .. i .. "|sleep 0.3" end
  return "STEPS:" .. table.concat(s, ";;")
end

local gap
-- `check` once a few more rows have arrived.
local function after_more(go, check)
  local from = rows()
  when(function() return rows() >= from + 3 end, function() check(rows() >= from + 3); go() end)
  return false
end
local steps = {
  -- A chat too short to scroll has nowhere to go: a nudge up doesn't stop it following.
  function(go)
    serpis.command.run("new-chat")
    serpis.command.run("chat-focus-transcript")
    local s = {}
    for i = 1, 30 do s[i] = "Short " .. i .. "|sleep 0.1" end
    serpis.chat.prompt(chat(), "STEPS:" .. table.concat(s, ";;"))
    when(function() return (serpis.chat.transcript() or ""):find("Short 1", 1, true) end, go)
    return false
  end,
  function(go)
    gap = behind()
    local v = d.cell_rect(last_seen(), 1)
    d.mouse_move(v.x + 40, v.y - 10)
    serpis.defer(100, function() d.wheel(20); serpis.defer(100, go) end)
    return false
  end,
  function(go)
    when(function() return not serpis.chat.running(chat()) end, go)
    return false
  end,
  function(go)
    T.check("nudged up while too short to scroll, it still follows", behind() <= gap + 2, true)
    go()
    return false
  end,
  function(go)
    serpis.command.run("new-chat")
    serpis.chat.prompt(chat(), "LONG")
    when(function() return (serpis.chat.transcript() or ""):find("Section 40", 1, true) and not serpis.chat.running(chat()) end, go)
    return false
  end,
  function(go)
    serpis.command.run("chat-focus-transcript")
    serpis.chat.prompt(chat(), run_steps(60))
    when(function() return (serpis.chat.transcript() or ""):find("Step 3", 1, true) end, go)
    return false
  end,
  function(go)
    gap = behind()
    return after_more(go, function(arrived)
      T.check("untouched, it follows", arrived and behind() <= gap + 1, true)
    end)
  end,
  function(go)
    local v = d.cell_rect(last_seen(), 1)
    d.mouse_move(v.x + 40, v.y - 60)
    serpis.defer(200, function() d.wheel(600); serpis.defer(1000, go) end)
    return false
  end,
  function(go)
    gap = behind()
    return after_more(go, function(arrived)
      T.check("scrolled up, it stays put as output arrives", arrived and behind() >= gap + 3, true)
    end)
  end,
  function(go)
    d.wheel(-100000)
    serpis.defer(1000, go)
    return false
  end,
  function(go)
    gap = behind()
    return after_more(go, function(arrived)
      T.check("scrolled back to the end, it follows again", arrived and behind() <= gap + 1, true)
    end)
  end,
  -- A little scrolled up is scrolled up: small moves aren't swallowed by the follow.
  function(go)
    d.wheel(3)
    serpis.defer(200, go)
    return false
  end,
  function(go)
    gap = behind()
    return after_more(go, function(arrived)
      T.check("a small move up lets go too", arrived and behind() >= gap + 3, true)
    end)
  end,
  -- And that one comes back the same way.
  function(go)
    d.wheel(-100000)
    serpis.defer(1000, go)
    return false
  end,
  function(go)
    gap = behind()
    return after_more(go, function(arrived)
      T.check("and back down it follows", arrived and behind() <= gap + 1, true)
    end)
  end,
}

local i = 0
local function go()
  i = i + 1
  local s = steps[i]
  if not s then return T.finish() end
  local wait = s(go)
  if wait ~= false then serpis.defer(wait or 300, go) end
end
go()
