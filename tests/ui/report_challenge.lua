-- Challenging a report, through real clicks and keys: a chat in a git checkout with uncommitted
-- work ends with a report; its header's Challenge sets the challenge up in the composer (model,
-- thinking, default from now on, a note), and RET starts it: a worktree with that work copied in,
-- the challenger there, recording challenge.md's findings with its tools (one turned down for a
-- quote not in the report, then fixed), and its findings in the chat's review under the claims
-- they dispute, the claims that held marked. One
-- is dismissed, one proven (Prove it), one gets a note; sent, they go first, under their own
-- heading. Once its chat is closed and its time is up (chat-report-challenge-worktree-ttl), the
-- worktree goes.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"},{"id":"other"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
local shots = os.getenv("SERPIS_TEST_SHOTS")
local function shot(name) if shots then d.screenshot(shots .. "/" .. name .. ".png", true) end end

-- A project with a commit, a change not committed and a file not added.
local home = os.getenv("HOME")
local proj = home .. "/proj"
local function sh(cmd) return os.execute(cmd) end
sh("mkdir -p '" .. proj .. "' && cd '" .. proj .. "' && git init -q && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m start"
  .. " && printf 'one\\n' > a.txt && git add a.txt && git -c user.name=t -c user.email=t@t commit -q -m a"
  .. " && printf 'two\\n' > a.txt && printf 'new\\n' > new.txt")
local function read(path)
  local h = io.open(path)
  if not h then return nil end
  local s = h:read("a")
  h:close()
  return s
end
local function exists(path) return read(path) ~= nil end
-- What the mock model was asked, newest last.
local function asked()
  local out = {}
  for line in (read(os.getenv("SERPIS_TEST_MOCK_LOG")) or ""):gmatch("[^\n]+") do
    local text = line:match('"text": "(.-)", "skills"')
    if text then out[#out + 1] = text end
  end
  return out
end
local function find_asked(prefix)
  for _, t in ipairs(asked()) do
    if t:sub(1, #prefix) == prefix then return t end
  end
end

-- The transcript line (1-based) holding `text`, and the column it starts at (in characters).
local function find(text)
  local n = 0
  for line in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
    n = n + 1
    local at = line:find(text, 1, true)
    if at then return n, select(2, line:sub(1, at - 1):gsub("[^\128-\191]", "")) + 1 end
  end
end
local function click_at(x, y)
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end
local function click(r) if r then click_at(r.x + r.w / 2, r.y + r.h / 2) end end
-- Click the words `label` on the line holding `text` (scrolled to first).
local function click_words(text, label)
  local line = find(text)
  if not line then return T.check("found " .. text, nil, "a line") end
  serpis.buffer.set_cursor(line, 1)
  serpis.defer(400, function()
    local _, col = find(text)
    local t = ""
    local n = 0
    for l in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
      n = n + 1
      if n == line then t = l end
    end
    local at = t:find(label, 1, true)
    local c = select(2, t:sub(1, at - 1):gsub("[^\128-\191]", "")) + 1
    local r = d.cell_rect(line, c + 1)
    if not r then return T.check("on screen: " .. label, nil, "a rect") end
    click(r)
  end)
end

local chat, run
local claim = "This is the likely cause of the flaky retries_respect_limit test in CI."
local steps = {
  function() serpis.command.run("new-chat", proj); return 600 end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    serpis.command.run("beginning-of-buffer")
    return 600
  end,
  function()
    local b = d.rect("report-challenge")
    T.check("the report's header has Challenge", b ~= nil, true)
    click(b)
    return 600
  end,
  function()
    local form = serpis.chat.review(chat).challenge_form
    T.check("Challenge sets it up in the composer", form ~= nil, true)
    T.check("on the chat's model", form and form.model, "mock/m")
    T.check("its keys are on", (table.concat(serpis.buffer.modes(), ",")):find("chat-challenging", 1, true) ~= nil, true)
    shot("1-form")
    -- Another model (the picker is serpis's usual one), thinking off, and default from now on.
    serpis.command.run("chat-challenge-form-model", "mock/other")
    click(d.rect("challenge-thinking"))
    return 500
  end,
  function()
    click(d.rect("challenge-remember"))
    return 400
  end,
  function()
    local form = serpis.chat.review(chat).challenge_form
    T.check("the model chosen", form and form.model, "mock/other")
    T.check("a click on Thinking: the next level", form and form.thinking, "off")
    T.check("default from now on, ticked", form and form.remember, true)
    d.text("Focus on the race.")
    return 400
  end,
  function()
    shot("2-form-filled")
    T.press("RET")
    return 300
  end,
  function()
    T.check("RET starts it", serpis.chat.review(chat).challenge_form, nil)
    T.check("the composer is empty again", d.composer_state().text, "")
    T.check("they are the default now", serpis.var.get("chat-report-challenge-model"), "mock/other")
    T.check("its thinking too", serpis.var.get("chat-report-challenge-thinking"), "off")
    return 200
  end,
  function()
    shot("3-running")
    T.check("the header shows it running, with a spinner", d.rect("report-challenge-spinner") ~= nil and d.rect("report-challenge-stop") ~= nil, true)
    T.check("and says so", d.rect("report-challenge-status") ~= nil, true)
    return 5000
  end,
  function()
    run = serpis.chat.challenges(chat)[1]
    T.check("a challenge was started", run ~= nil, true)
    if not run then return T.finish() end
    T.check("on the model asked for", run.model, "mock/other")
    T.check("it has ended", run.running, false)
    T.check("in a worktree of its own", run.branch:match("^challenge%-") ~= nil and exists(run.worktree .. "/a.txt"), true)
    T.check("with the chat's uncommitted change", read(run.worktree .. "/a.txt"), "two\n")
    T.check("and its new file", read(run.worktree .. "/new.txt"), "new\n")
    T.check("the chat's checkout is untouched", read(proj .. "/a.txt"), "two\n")
    local sent = find_asked('<skill name=\\"challenge\\"') or ""
    T.check("the challenger gets the challenge skill", sent ~= "", true)
    T.check("and the report", sent:find("retries_respect_limit", 1, true) ~= nil, true)
    T.check("and where it is", sent:find(run.worktree, 1, true) ~= nil, true)
    T.check("and what you added", sent:find("Focus on the race.", 1, true) ~= nil, true)
    T.check("its findings are counted", run.findings, 3)
    T.check("and the claims that held", table.concat(run.held, ","), "Changed 2,Checked 1")
    local r = serpis.chat.review(chat)
    T.check("they wait in the review", r.drafts, 3)
    local a, b = r.comments[1], r.comments[3]
    T.check("the first under the words it quotes", a and a.quote, claim)
    T.check("its place", a and a.label, "Changed 1")
    T.check("confirmed", a and a.challenge and a.challenge.verdict, "confirmed")
    T.check("with its evidence", a and a.challenge and a.challenge.evidence, "`retries_respect_limit` with 8 workers failed 3 of 500 runs.")
    T.check("a quote the tool turned down, fixed: the words it then quoted", b and b.quote, "passed every time.")
    local told = false
    for line in (read(os.getenv("SERPIS_TEST_MOCK_LOG")) or ""):gmatch("[^\n]+") do
      if line:find("The quote isn't in the report", 1, true) then told = true end
    end
    T.check("the tool told it its first quote wasn't the report's", told, true)
    T.check("plausible", b and b.challenge and b.challenge.verdict, "plausible")
    local t = serpis.chat.transcript(chat) or ""
    T.check("its card is under the claim", t:find("Challenge   CONFIRMED   ·  draft · click to add a note", 1, true) ~= nil, true)
    T.check("with the finding", t:find("The race is still there: push_front runs before the counter is stored.", 1, true) ~= nil, true)
    T.check("the header says how it went", d.rect("report-challenge-again") ~= nil and d.rect("report-challenge-open") ~= nil, true)
    serpis.command.run("chat-focus-transcript")
    serpis.command.run("beginning-of-buffer")
    return 600
  end,
  function()
    shot("4-findings")
    -- Dismiss the one about Changed 3: its card in view first.
    local line = find("A test helper in tests/common.rs still calls it.")
    if line then serpis.buffer.set_cursor(line, 1) end
    return 400
  end,
  function()
    local line = find("A test helper in tests/common.rs still calls it.")
    -- Its card's head is the line above.
    local head = line and select(1, (function()
      local n, t = 0, nil
      for l in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
        n = n + 1
        if n == line - 1 then t = l end
      end
      return t
    end)())
    T.check("its card offers dismiss", head and head:find("dismiss", 1, true) ~= nil, true)
    if head then click_words(head, "dismiss") end
    return 900
  end,
  function()
    local r = serpis.chat.review(chat)
    T.check("dismissed: two left", r.drafts, 2)
    -- Prove the plausible one left (Checked 2).
    local line = find("The loop ran on one thread, which can't hit a race between workers.")
    local head
    local n = 0
    for l in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
      n = n + 1
      if n == (line or 0) - 1 then head = l end
    end
    T.check("a plausible finding offers prove it", head and head:find("prove it", 1, true) ~= nil, true)
    if head then click_words(head, "prove it") end
    return 3000
  end,
  function()
    local r = serpis.chat.review(chat)
    local c = r.comments[#r.comments]
    T.check("proven: now confirmed", c and c.challenge and c.challenge.verdict, "confirmed")
    T.check("with what it ran", c and c.challenge and c.challenge.evidence, "3 of 500 runs failed with 8 threads.")
    local sent = find_asked("Prove this finding") or ""
    T.check("the challenger was asked about it", sent:find("Checked 2", 1, true) ~= nil, true)
    serpis.command.run("beginning-of-buffer")
    return 400
  end,
  function()
    shot("5-proven")
    -- A note on the first: its card, in the composer.
    serpis.command.run("chat-comment-edit", serpis.chat.review(chat).comments[1].id)
    return 300
  end,
  function()
    d.text("Agreed. Add the run to the test.")
    return 400
  end,
  function()
    serpis.command.run("chat-comment-save")
    return 300
  end,
  function()
    T.check("the note is kept with the finding", serpis.chat.review(chat).comments[1].text, "Agreed. Add the run to the test.")
    T.check("still a challenge", serpis.chat.review(chat).comments[1].challenge ~= nil, true)
    serpis.command.run("chat-send")
    return 2500
  end,
  function()
    local sent = find_asked("Review comments on what you wrote.") or ""
    T.check("sent, challenges come first", sent:find("\\n\\n## Challenged by another agent\\n", 1, true) ~= nil, true)
    T.check("with your note", sent:find("My note: Agreed. Add the run to the test.", 1, true) ~= nil, true)
    T.check("the review is empty again", serpis.chat.review(chat).drafts, 0)
    local t = serpis.chat.transcript(chat) or ""
    T.check("your message shows them", t:find("Review  ·  2 challenges", 1, true) ~= nil, true)
    T.check("the cards say they were answered", t:find("Challenge   CONFIRMED   ·  sent  ·  answered ↓", 1, true) ~= nil, true)
    shot("2-sent")
    -- Its chat closed and its time up: the worktree goes.
    serpis.var.set("chat-report-challenge-worktree-ttl", 1)
    local b = serpis.chat.challenges(chat)[1].challenger_buffer
    T.check("its chat is open", b ~= nil, true)
    if b then serpis.buffer.kill(b) end
    return 4000
  end,
  function()
    T.check("the worktree is removed", exists(run.worktree .. "/a.txt"), false)
    T.check("and the challenge forgotten", #serpis.chat.challenges(chat), 0)
    local branches = io.popen("git -C '" .. proj .. "' branch --list 'challenge-*'"):read("a")
    T.check("with its branch", branches, "")
    T.finish()
  end,
}

local i = 0
local function go()
  i = i + 1
  local step = steps[i]
  if not step then return end
  local ok, wait = pcall(step)
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
