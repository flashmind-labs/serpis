-- A review sent with your message is drawn as one in your bubble (what it asks, where, linked
-- back); the comments sent stay on the report, each linked to the agent's answer once it quotes
-- them, and that quote in the answer links back. A diff page's review is drawn as a review too.
-- report_replies_restore runs next: it's all read from the transcript, so it comes back.
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
-- Scrolling jumps: a drag aimed at text must land on it, not where it scrolled to.
serpis.var.set("scroll-animation-time", 0)

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
-- A quote in the agent's answer that links back to what it is about.
local function answer_quote(l) return l.bar ~= nil and l.links[1] ~= nil and l.links[1]:sub(1, 5) == "find:" end
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


-- The drawn line (1-based) that `pred` picks, and it.
local function line_where(pred)
  for i, l in ipairs(d.chat_lines() or {}) do
    if pred(l) then return i, l end
  end
end
local function cursor_line() return (serpis.buffer.cursor()) end
-- Click character `col` (1-based) of drawn line `n`, bringing it on screen first.
local function click_cell(n, col)
  serpis.buffer.set_cursor(n, 1)
  serpis.defer(500, function()
    local r = d.cell_rect(n, col)
    if r then click(r) else T.check("on screen: line " .. n, nil, "a rect") end
  end)
end

local first = "This is the likely cause of the flaky retries_respect_limit test in CI."
local second = "Retries from many clients were synchronizing after an outage."
local report_line, answer_line

local steps = {
  function() serpis.command.run("new-chat"); return 600 end,
  function() chat = serpis.buffer.current(); serpis.chat.prompt(chat, "REPORT:skill"); return 2500 end,
  function() serpis.command.run("chat-focus-transcript"); drag_select(first); return 1200 end,
  function() T.press("a"); return 400 end,
  function() T.press(keys("Did you reproduce it?")); return 300 end,
  function() T.press("RET"); return 400 end,
  function() serpis.command.run("chat-focus-transcript"); drag_select(second); return 1200 end,
  function() T.press("c"); return 400 end,
  function() T.press(keys("Show the logs.")); return 300 end,
  function() T.press("RET"); return 400 end,
  function() T.press(keys("Then run the tests again.")); return 300 end,
  function() T.press("RET"); return 3000 end,
  function()
    shot("1-sent")
    T.check("sent, nothing waits", serpis.chat.review(chat).drafts, 0)
    -- Your bubble: the review as one.
    T.check("your message is drawn as a review", line_where(function(l) return l.text == "Review  ·  1 question, 1 comment" end) ~= nil, true)
    local _, place = line_where(function(l) return l.text == "Question   ↑ Changed 1" end)
    T.check("each comment says where, as a link", place and place.links[1] and place.links[1]:sub(1, 5), "find:")
    T.check("what it quotes", line_where(function(l) return l.text == "│ " .. first end) ~= nil, true)
    T.check("what was typed with it, last", line_where(function(l) return l.text == "Then run the tests again." end) ~= nil, true)
    -- The report: the comments sent stay, linked to their answers.
    local q, ql = line_where(function(l) return l.text == "Question  ·  sent  ·  answered ↓" end)
    T.check("a question sent stays on the report, answered", q ~= nil and ql.links[1] ~= nil, true)
    T.check("a comment too", line_where(function(l) return l.text == "Comment  ·  sent  ·  answered ↓" end) ~= nil, true)
    report_line = line_where(function(l) return l.text:find(first, 1, true) and l.marks > 0 and not l.text:find("│", 1, true) and not answer_quote(l) end)
    T.check("what it is about stays marked", report_line ~= nil, true)
    -- The answer: its quotes marked, linked back.
    answer_line = line_where(function(l) return answer_quote(l) and l.text:find("This is the likely cause", 1, true) end)
    T.check("the answer's quote of it is marked", answer_line ~= nil, true)
    T.check("both answered quotes", select(2, (function() local n = 0 for _, l in ipairs(d.chat_lines()) do if answer_quote(l) then n = n + 1 end end return nil, n end)()), 2)
    -- Follow the links: card → answer.
    serpis.command.run("chat-focus-transcript")
    local text = ql and ql.text or ""
    local col = select(2, text:sub(1, (text:find("answered", 1, true) or 1) - 1):gsub("[^\128-\191]", "")) + 3
    click_cell(q, col)
    return 1500
  end,
  function()
    T.check("“answered ↓” goes to the answer", cursor_line(), answer_line)
    shot("2-answer")
    click_cell(answer_line, 3)
    return 1500
  end,
  function()
    T.check("the answer's quote goes back to what it is about", cursor_line(), report_line)
    shot("3-report")
    local n = line_where(function(l) return l.text == "Question   ↑ Changed 1" end)
    click_cell(n, 14)
    return 1500
  end,
  function()
    T.check("so does the place in your review", cursor_line(), report_line)
    -- A diff page's review, as it sends it: drawn as a review too.
    serpis.chat.prompt(chat, "Review comments on main → the worktree in /w. Line numbers are the current file's, except where a comment says it is on the old version.\n\n## Fix\nFix these.\n\n`src/a.rs:12-13`\n```rs\nlet x = 1;\nlet y = 2;\n```\nUse a constant.\n\n## Questions\nAnswer these.\n\n`src/a.rs:30` (on the old version)\nWhy run it here?\n")
    return 2500
  end,
  function()
    shot("4-diff-review")
    T.check("a diff page's review is drawn as one", line_where(function(l) return l.text == "Review of main → the worktree  ·  1 question, 1 fix" end) ~= nil, true)
    local _, fix = line_where(function(l) return l.text == "Fix   src/a.rs:12-13" end)
    T.check("its places are links to the code", fix and fix.links[1], "src/a.rs:12-13")
    T.check("with the code it is about", line_where(function(l) return l.text == "│ let x = 1;" end) ~= nil, true)
    T.check("the old version noted", line_where(function(l) return l.text == "Question   src/a.rs:30 (on the old version)" end) ~= nil, true)
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
