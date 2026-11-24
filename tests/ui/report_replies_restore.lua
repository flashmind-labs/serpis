-- Runs after report_replies with the same config: a review sent and its answers are read from
-- the transcript, so after starting again they show as before.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function has(text)
  for _, l in ipairs(d.chat_lines() or {}) do if l.text == text then return true end end
  return false
end
local steps = {
  function()
    for _, b in ipairs(serpis.buffer.list()) do
      if b.kind == "chat" and (serpis.chat.transcript(b.id) or ""):find("Needs you", 1, true) then serpis.buffer.show(b.id) end
    end
    return 1500
  end,
  function()
    T.check("the review you sent is still drawn as one", has("Review  ·  1 question, 1 comment"), true)
    T.check("its comments still on the report", has("Question  ·  sent  ·  answered ↓"), true)
    T.check("both", has("Comment  ·  sent  ·  answered ↓"), true)
    local n = 0
    for _, l in ipairs(d.chat_lines()) do if l.marker == "↑" then n = n + 1 end end
    T.check("and the answers' quotes linked back", n, 2)
    T.finish()
    return 100000
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return end
  local ok, wait = pcall(steps[i])
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
