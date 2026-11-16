-- The state and saved sessions left by pins.lua survive a full application restart.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local expected = io.open(os.getenv("SERPIS_TEST_OUT"):gsub("pins_restore%.out$", "pins.out.pin"), "r")
local first_id = expected and expected:read("*a") or ""
if expected then expected:close() end

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local step = list[i]
    if not step then return T.finish() end
    serpis.defer(step() or 400, go)
  end
  go()
end

steps({
  function()
    serpis.command.run("session-rescan")
    return 1000
  end,
  function()
    local pins = serpis.chat.pinned()
    T.check("pins persist across restart", #pins, 2)
    T.check("pin order persists", pins[1] and pins[1].id, first_id)
    T.check("closed pin resolves saved session", pins[1] and pins[1].available, true)
    serpis.command.run("chat-pin-open", 1)
    return 700
  end,
  function()
    T.check("closed pin resumes a chat", serpis.buffer.kind(), "chat")
    T.check("resumed stable identity", serpis.chat.info().session_id, first_id)
    T.check("transcript restored", (serpis.buffer.text() or ""):find("first priority", 1, true) ~= nil, true)
    T.check("unpin API removes it", serpis.chat.unpin(), true)
    T.check("one pin remains", #serpis.chat.pinned(), 1)
  end,
})
