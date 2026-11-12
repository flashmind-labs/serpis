-- Reviewing what an agent wrote: select part of its report, Ask (the bar beside the selection)
-- or press c, write the comment in the composer and save it to the review; its card edits it.
-- Quits in the middle of a third comment, with a message typed: report_review_restore runs next
-- and finds it all again, then sends the review.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)

local chat
local function keys(text)
  local out = {}
  for c in text:gmatch(".") do out[#out + 1] = c == " " and "SPC" or c end
  return table.concat(out, " ")
end
-- The transcript line (1-based) holding `text`, and the column it starts at.
local function find(text)
  local n = 0
  for line in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
    n = n + 1
    local at = line:find(text, 1, true)
    -- Columns count characters (the line may have "±" before it).
    if at then return n, select(2, line:sub(1, at - 1):gsub("[^\128-\191]", "")) + 1 end
  end
end
local function center(r) return r.x + r.w / 2, r.y + r.h / 2 end
local function click(r)
  local x, y = center(r)
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end
-- Select `text` in the transcript with a drag, as a person does.
local function drag_select(text)
  local line, col = find(text)
  if not line then return T.check("found " .. text, nil, "a line") end
  serpis.buffer.set_cursor(line, col)
  serpis.defer(400, function()
    local a, b = d.cell_rect(line, col), d.cell_rect(line, col + #text - 1)
    if not (a and b) then return T.check("on screen: " .. text, nil, "rects") end
    local x0, y0 = a.x + 1, a.y + a.h / 2
    local x1, y1 = b.x + b.w - 1, b.y + b.h / 2
    d.mouse_move(x0, y0)
    serpis.defer(80, function()
      d.mouse_button(x0, y0, "left", true)
      serpis.defer(80, function()
        d.mouse_move((x0 + x1) / 2, (y0 + y1) / 2)
        serpis.defer(80, function()
          d.mouse_move(x1, y1)
          serpis.defer(80, function() d.mouse_button(x1, y1, "left", false) end)
        end)
      end)
    end)
  end)
end
local function review() return serpis.chat.review(chat) end
-- SERPIS_TEST_SHOTS=dir: screenshots of each stage, to look at.
local shots = os.getenv("SERPIS_TEST_SHOTS")
local function shot(name) if shots then d.screenshot(shots .. "/" .. name .. ".png", true) end end
local function composer() return d.composer_state().text end

local first = "This is the likely cause of the flaky retries_respect_limit test in CI."
local second = "Retries from many clients were synchronizing after an outage."
local third = "With the current backoff I expect this is fine."

local steps = {
  function() serpis.command.run("new-chat"); return 600 end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    drag_select(first)
    return 1200
  end,
  function()
    local ask = d.rect("chat-selection-ask")
    T.check("a selection in the report shows the bar", ask ~= nil, true)
    shot("1-selection")
    if ask then serpis.defer(300, function() click(ask) end) end
    return 1000
  end,
  function()
    local w = review().writing
    T.check("Ask starts a question", w and w.kind, "question")
    T.check("about the selection", w and w.quote, first)
    T.check("where it is in the report", w and w.label, "Changed 1")
    T.press(keys("Did you reproduce it?"))
    return 300
  end,
  function()
    shot("2-writing")
    serpis.defer(300, function() T.press("RET") end)
    return 800
  end,
  function()
    local r = review()
    T.check("RET saves it to the review", r.drafts, 1)
    T.check("with what was written", r.comments[1] and r.comments[1].text, "Did you reproduce it?")
    T.check("the composer is free again", composer(), "")
    T.check("its card shows under what it is about", find("Question  ·  draft · click to change") ~= nil, true)
    -- Its card: a click changes it; ESC leaves it as it was.
    serpis.command.run("chat-focus-transcript")
    local line = find("Question  ·  draft")
    if line then serpis.buffer.set_cursor(line, 1) end
    return 600
  end,
  function()
    local line = find("Question  ·  draft")
    local r1 = line and d.cell_rect(line, 2)
    T.check("its card is on screen", r1 ~= nil, true)
    if r1 then click(r1) end
    return 600
  end,
  function()
    local w = review().writing
    T.check("a click on its card changes it", w and w.id, review().comments[1].id)
    T.check("in the composer", composer(), "Did you reproduce it?")
    T.press("ESC")
    return 400
  end,
  function()
    T.check("ESC leaves it as it was", review().writing, nil)
    T.check("still saved", review().drafts, 1)
    serpis.command.run("chat-focus-transcript")
    drag_select(second)
    return 1200
  end,
  function()
    T.press("c")
    return 500
  end,
  function()
    local w = review().writing
    T.check("c on a selection asks for a change", w and w.kind, "fix")
    T.check("its place", w and w.label, "Changed 2")
    T.check("exactly what was selected", w and w.quote, second)
    T.press(keys("Show the logs."))
    return 300
  end,
  function() T.press("RET"); return 500 end,
  function()
    T.check("two comments now", review().drafts, 2)
    -- A message typed meanwhile, then a third comment begun: quitting keeps both.
    T.press(keys("Then run the tests again."))
    return 600
  end,
  function()
    shot("3-review")
    local toggle = d.rect("review-toggle")
    if toggle then serpis.defer(300, function() click(toggle) end) end
    return 900
  end,
  function()
    shot("4-review-open")
    return 400
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    drag_select(third)
    return 1200
  end,
  function() T.press("a"); return 500 end,
  function()
    local w = review().writing
    T.check("a third, in Needs you", w and w.label, "Needs you 3")
    T.press(keys("Why?"))
    return 1500
  end,
  function()
    T.check("being written, it is kept", review().writing and review().writing.text, "Why?")
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
