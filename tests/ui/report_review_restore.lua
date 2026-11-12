-- Runs after report_review with the same config: the review is all there after starting again
-- (two saved comments, a third being written, the message typed before it), and goes to the
-- agent as one message with that message.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local log = os.getenv("SERPIS_TEST_MOCK_LOG")
local function keys(text)
  local out = {}
  for c in text:gmatch(".") do out[#out + 1] = c == " " and "SPC" or c end
  return table.concat(out, " ")
end
local function last_request()
  local last
  local fh = io.open(log)
  if fh then
    for l in fh:lines() do last = l end
    fh:close()
  end
  return last or ""
end
local function review() return serpis.chat.review() end
local function composer() return d.composer_state().text end

local steps = {
  function()
    -- The chat with the report (starting again opens on a new one).
    for _, b in ipairs(serpis.buffer.list()) do
      if b.kind == "chat" and (serpis.chat.transcript(b.id) or ""):find("Needs you", 1, true) then serpis.buffer.show(b.id) end
    end
    return 1500
  end,
  function()
    T.check("the chat with the report is back", (serpis.chat.transcript() or ""):find("Needs you", 1, true) ~= nil, true)
    local r = review()
    T.check("its saved comments too", r.drafts, 2)
    T.check("and the one being written", r.writing and r.writing.text, "Why?")
    T.check("where it was", r.writing and r.writing.label, "Needs you 3")
    T.check("in the composer", composer(), "Why?")
    T.press(keys(" At 50 rps?"))
    return 300
  end,
  function() T.press("RET"); return 500 end,
  function()
    T.check("saved", review().drafts, 3)
    T.check("the message typed before it is back", composer(), "Then run the tests again.")
    T.press("RET")
    return 2500
  end,
  function()
    local req = last_request()
    for _, want in ipairs({ "## Questions", "> This is the likely cause of the flaky retries_respect_limit test in CI.", "Did you reproduce it?", "Needs you 3:", "Why? At 50 rps?", "## Changes asked for", "Show the logs.", "## Also", "Then run the tests again." }) do
      T.check("the agent gets: " .. want, req:find(want, 1, true) ~= nil, true)
    end
    T.check("sent, nothing waits", review().drafts, 0)
    local sent = 0
    for _, c in ipairs(review().comments) do if c.state == "sent" then sent = sent + 1 end end
    T.check("its comments show as sent", sent, 3)
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
