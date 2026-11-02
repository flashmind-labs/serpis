-- The diff page: what a branch and uncommitted work look like, compared by tokens; moving through
-- changes across files; comments written with real keys and the mouse; a review reaching a chat's
-- agent (the mock model) and the code changing under the comments. Everything in a throwaway repo.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local function sh(cmd) local p = io.popen(cmd .. " 2>&1"); local o = p:read("*a"); p:close(); return (o:gsub("%s+$", "")) end
local function write(path, text) local f = io.open(path, "w"); f:write(text); f:close() end
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
write(agent_dir .. "/models.json", string.format('{"providers":{"mock":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"k","compat":{"openRouter":true},"models":[{"id":"anthropic/claude-sonnet-4.5","reasoning":false,"input":["text"],"cost":{"input":1,"output":1,"cacheRead":0.1,"cacheWrite":1},"contextWindow":200000,"maxTokens":4096}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
serpis.agent.reload_models()
serpis.agent.model("mock/anthropic/claude-sonnet-4.5")
serpis.var.set("session-directory", agent_dir .. "/sessions")

local repo = sh("mktemp -d /tmp/sdiff.XXXXXX")
repo = sh("cd '" .. repo .. "' && pwd -P")
local git = "git -c user.email=t@t -c user.name=t"
local function in_repo(cmd) return sh("cd '" .. repo .. "' && " .. cmd) end
sh("mkdir -p '" .. repo .. "/src'")
local base_rs = [[
fn total(price: u32, count: u32) -> u32 {
    let value = price * count;
    value
}

fn unrelated_one() {
    first();
    second();
    third();
    fourth();
    fifth();
    sixth();
    seventh();
    eighth();
}

fn call_site() {
    run(alpha, beta, gamma);
}
]]
write(repo .. "/src/a.rs", base_rs)
write(repo .. "/notes.txt", "keep\nthis\n")
in_repo("git init -q -b main && git add . && " .. git .. " commit -qm init && git checkout -qb work")
-- A renamed variable, a call wrapped over lines, and a new function.
write(repo .. "/src/a.rs", (base_rs:gsub("let value = price %* count;\n    value", "let amount = price * count;\n    amount"):gsub("run%(alpha, beta, gamma%);", "run(\n        alpha,\n        beta,\n        gamma\n    );")) .. "\nfn added_later() {\n    new_code();\n}\n")
in_repo("git add . && " .. git .. " commit -qm work")
-- Uncommitted: a new file and an edit.
write(repo .. "/src/b.py", "def hello():\n    return 1\n")
write(repo .. "/notes.txt", "keep\nthat\n")

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(250, go)
end
local function info() return serpis.diff.info() end
local function page() return serpis.buffer.text() or "" end
local function has(s) return page():find(s, 1, true) ~= nil end
local function shot(name, go)
  local shots = os.getenv("SERPIS_TEST_SHOTS")
  if not shots then return true end
  serpis.defer(700, function() d.screenshot(shots .. "/" .. name .. ".png"); serpis.defer(300, go) end)
  return false
end
local function click(name, go)
  local r = d.composer()[name]
  if not r then
    T.check("control " .. name .. " is drawn", false, true)
    return false
  end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
  return true
end
-- Room for the file list and both sides in the test's window.
serpis.var.set("diff-tree-width", 170)
serpis.var.set("diff-split-min-width", 500)
local chat, vim_was, notrepo
local steps = {
  function(go)
    chat = serpis.buffer.create("chat", "*c*", { cwd = repo })
    serpis.buffer.show(chat)
    serpis.defer(300, function()
      T.press("s-D")
      when(function() return serpis.buffer.kind() == "diff" and info().loaded and info().file ~= nil and not info().loading and info().rows > 0 end, go)
    end)
    return false
  end,
  function(go)
    T.check("on a branch it opens against the main branch", info().compared, "main → worktree")
    T.check("the page is laid out by a key from a chat", serpis.buffer.kind(), "diff")
    local names = {}
    for _, f in ipairs(serpis.diff.files()) do names[#names + 1] = f.status .. " " .. f.path end
    T.check("committed and uncommitted work, untracked files too", table.concat(names, ","), "M notes.txt,M src/a.rs,A src/b.py")
    T.check("the first file is showing", info().file, "notes.txt")
    T.check("the chat it came from gets the review", info().target ~= nil, true)
    T.check("the change is marked", has("- 2 - | this") and has("+ - 2 | that"), true)
    return shot("diff-notes", go)
  end,
  function(go)
    T.press("n")
    when(function() return info().file == "src/a.rs" and info().rows > 0 end, go)
    return false
  end,
  function(go)
    T.check("n goes on into the next file", info().file, "src/a.rs")
    T.check("a rename is one change", has("let value = price") and has("let amount = price"), true)
    T.check("unchanged stretches fold", has("unchanged lines"), true)
    T.check("wrapping a call is only formatting", has("~ 18 - |     run(alpha, beta, gamma);"), true)
    T.check("n p see 2 real changes here (the wrap isn't one)", info().changes, 2)
    return shot("diff-rust", go)
  end,
  function()
    T.press("n")
  end,
  function()
    T.check("n stops at the added function, past the formatting", info().cursor > 10 and has("fn added_later"), true)
    T.press("p")
  end,
  function()
    T.check("p goes back", info().change, 1)
    T.press("x")
  end,
  function()
    T.check("x shows everything", has("unchanged lines"), false)
    T.press("x")
  end,
  function()
    T.check("x folds again", has("unchanged lines"), true)
    T.press("s")
  end,
  function(go)
    T.check("s switches to unified", info().view, "unified")
    return shot("diff-unified", go)
  end,
  function()
    T.press("s")
  end,
  function()
    T.check("and back", info().view, "split")
    T.press("w")
  end,
  function()
    T.check("w: spacing hides", info().whitespace, "hide")
  end,
  function()
    T.check("formatting looks unchanged, and folds", has("~ 18"), false)
    T.press("w")
  end,
  function()
    T.check("w: every byte counts", info().whitespace, "exact")
    T.check("exact shows the wrap as a change", has("- 18 - |     run(alpha, beta, gamma);"), true)
    T.press("w")
  end,
  -- A comment with the keyboard.
  function(go)
    T.check("w: back to smart", info().whitespace, "smart")
    T.press("g g")
    T.press("n")
    serpis.defer(200, function()
      T.press("c")
      when(function() return info().writing ~= nil end, go)
    end)
    return false
  end,
  function()
    T.check("c opens the composer on the change", info().writing.side, "new")
    T.check("typing reaches it, the page's keys don't", serpis.buffer.kind(), "diff-comment")
    d.text("use a better name")
    T.press("RET")
    d.text("for this")
  end,
  function(go)
    T.check("what is typed is in it, with its line break", info().writing.text, "use a better name\nfor this")
    return shot("diff-composer", go)
  end,
  function()
    T.press("C-RET")
  end,
  function(go)
    T.check("C-RET keeps it", info().writing, nil)
    local c = serpis.diff.comments()[1]
    T.check("on its line", c.path .. ":" .. c.line, "src/a.rs:2")
    T.check("with the code", c.code[1], "    let amount = price * count;")
    T.check("waiting to send", c.state, "draft")
    T.check("it shows between the lines", has("[fix draft] src/a.rs:2: use a better name"), true)
    T.check("the page's keys work again", info().drafts, 1)
    return shot("diff-comment", go)
  end,
  -- A question with the mouse: a click on a line number.
  function(go)
    local key = "gutter-new-3"
    if not click(key) then return T.finish() end
    when(function() return info().writing ~= nil end, go)
    return false
  end,
  function(go)
    T.check("a click on a line number starts a comment there", info().writing.line, 3)
    click("ask-question")
    when(function() return info().writing and info().writing.kind == "question" end, go)
    return false
  end,
  function(go)
    d.text("why return it")
    serpis.defer(200, function()
      click("comment-save")
      when(function() return info().writing == nil end, go)
    end)
    return false
  end,
  function(go)
    T.check("the button keeps it", #serpis.diff.comments(), 2)
    T.check("as a question", serpis.diff.comments()[2].kind, "question")
    T.check("two to send", info().drafts, 2)
    return shot("diff-review", go)
  end,
  -- Dragging over line numbers comments on those lines.
  function()
    T.press("G")
  end,
  function(go)
    local a, b = d.composer()["gutter-new-25"], d.composer()["gutter-new-27"]
    if not (a and b) then
      T.check("the added lines are on screen", false, true)
      return true
    end
    d.mouse_move(a.x + 8, a.y + 4)
    serpis.defer(100, function()
      d.mouse_button(a.x + 8, a.y + 4, "left", true)
      serpis.defer(100, function()
        d.mouse_move(b.x + 8, b.y + 4)
        serpis.defer(100, function()
          d.mouse_move(b.x + 8, b.y + 8)
          serpis.defer(100, function()
            d.mouse_button(b.x + 8, b.y + 8, "left", false)
            when(function() return info().writing ~= nil end, go)
          end)
        end)
      end)
    end)
    return false
  end,
  function(go)
    local w = info().writing
    T.check("a drag over line numbers comments on those lines", w and (w.line .. "-" .. w.end_line), "25-27")
    T.press("ESC")
    when(function() return info().writing == nil end, go)
    return false
  end,
  -- Keys select too; a click opens a fold.
  function()
    T.press("g g")
    T.press("J")
    T.press("J")
  end,
  function()
    local s = info().selection
    T.check("J extends the selection", s and (s.line .. "-" .. s.end_line), "1-3")
    T.press("j")
  end,
  function(go)
    T.check("moving drops it", info().selection.end_line == info().selection.line, true)
    local fold
    for name in pairs(d.composer()) do
      if name:match("^fold%-") then fold = name end
    end
    if not fold then
      T.check("a fold is drawn", false, true)
      return true
    end
    click(fold)
    when(function() return not has("unchanged lines") end, go)
    return false
  end,
  function()
    T.check("a click opens the fold", has("eighth();"), true)
    T.press("x")
  end,
  function()
    T.press("x")
  end,
  -- Escape drops a comment.
  function(go)
    T.press("k")
    T.press("c")
    when(function() return info().writing ~= nil end, go)
    return false
  end,
  function()
    d.text("nevermind")
    T.press("ESC")
  end,
  function()
    T.check("ESC drops it", info().writing == nil and #serpis.diff.comments() == 2, true)
  end,
  -- Comments survive reading the file again; the code moving takes them along.
  function(go)
    write(repo .. "/src/a.rs", "// a new first line\n" .. io.open(repo .. "/src/a.rs"):read("*a"))
    when(function() return has("// a new first line") and serpis.diff.comments()[1].line >= 2 end, go)
    return false
  end,
  function()
    T.check("the page noticed the file changed, and the comment moved with its code", serpis.diff.comments()[1].line, 3)
        T.check("the new line shows", has("// a new first line"), true)
  end,
  -- The review goes to the chat it came from.
  function(go)
    T.press("S")
    when(function() return info().drafts == 0 end, go)
    return false
  end,
  function(go)
    T.check("S sends them", serpis.diff.comments()[1].state, "sent")
    T.check("the keys stay on the page", serpis.buffer.kind(), "diff")
    local shown = false
    for _, b in ipairs(serpis.window.buffers()) do shown = shown or b == chat end
    T.check("the chat shows beside it", shown, true)
    when(function() return serpis.chat.transcript(chat):find("use a better name", 1, true) ~= nil end, go)
    return false
  end,
  function(go)
    local tr = serpis.chat.transcript(chat)
    T.check("the agent gets the comment", tr:find("use a better name", 1, true) ~= nil, true)
    T.check("and where it is", tr:find("src/a.rs:3", 1, true) ~= nil, true)
    T.check("and what to do", tr:find("Make these changes", 1, true) ~= nil, true)
    T.check("questions apart", tr:find("Answer these", 1, true) ~= nil, true)
    T.check("quoted code", tr:find("let amount = price", 1, true) ~= nil, true)
    return shot("diff-sent", go)
  end,
  -- Other comparisons, by the page's controls and by the picker.
  function(go)
    serpis.command.run("delete-other-windows")
    click("uncommitted")
    when(function() return info().compared == "Uncommitted changes" and info().loaded end, go)
    return false
  end,
  function()
    local n = {}
    for _, f in ipairs(serpis.diff.files()) do n[#n + 1] = f.path end
    T.check("Uncommitted leaves out what is committed", table.concat(n, ","), "notes.txt,src/a.rs,src/b.py")
    T.press("d")
  end,
  function(go)
    T.check("d asks what to compare", d.minibuffer() ~= nil, true)
    d.text("main..work")
    serpis.defer(200, function()
      T.press("RET")
      when(function() return info().compared == "main → work" and info().loaded end, go)
    end)
    return false
  end,
  function()
    local n = {}
    for _, f in ipairs(serpis.diff.files()) do n[#n + 1] = f.path end
    T.check("two refs: only what is committed", table.concat(n, ","), "src/a.rs")
    serpis.command.run("diff-select-source", "nope-not-a-branch")
  end,
  function(go)
    when(function() return info().error ~= nil end, go)
    return false
  end,
  function(go)
    T.check("a branch that isn't there says so", (info().error or ""):find("nope-not-a-branch", 1, true) ~= nil, true)
    serpis.command.run("diff-select-source", "uncommitted")
    when(function() return info().loaded and info().error == nil end, go)
    return false
  end,
  -- Vim's keys don't get in the way.
  function(go)
    serpis.keys.scheme("vim")
    serpis.buffer.show(serpis.buffer.current())
    T.press("]")
    when(function() return info().file == "src/a.rs" end, go)
    return false
  end,
  function()
    vim_was = info().cursor
    T.press("j")
  end,
  function()
    T.check("vim: j moves the page's cursor", info().cursor > vim_was, true)
    T.press("c")
  end,
  function()
    d.text("hello")
    T.press("ESC")
  end,
  function()
    T.check("vim: insert in the composer, ESC to normal", info().writing and info().writing.text, "hello")
    T.press("ESC")
  end,
  function()
    T.check("vim: ESC again drops it", info().writing, nil)
    serpis.keys.scheme("regular")
    T.press("v")
  end,
  function(go)
    local viewed = false
    for _, f in ipairs(serpis.diff.files()) do viewed = viewed or (f.path == "src/a.rs" and f.viewed) end
    T.check("v marks it viewed", viewed, true)
    T.check("and goes to the next file", info().file, "src/b.py")
    when(function() return not info().loading end, go)
    return false
  end,
  function(go)
    T.check("a new file is all new", has("+ - 1 | def hello():"), true)
    return shot("diff-new-file", go)
  end,
  function(go)
    write(repo .. "/src/a.rs", "// changed again\n")
    when(function()
      for _, f in ipairs(serpis.diff.files()) do
        if f.path == "src/a.rs" then return not f.viewed end
      end
    end, go)
    return false
  end,
  function()
    T.check("a file that changes after being viewed needs another look", true, true)
    serpis.buffer.kill(serpis.buffer.current())
  end,
  function(go)
    -- The review was saved: a new page for this worktree still has it.
    serpis.diff.open({ dir = repo })
    when(function() return serpis.buffer.kind() == "diff" and info().root == repo and #serpis.diff.comments() == 2 end, go)
    return false
  end,
  function()
    T.check("comments outlive the page", serpis.diff.comments()[2].text, "why return it")
    -- Not a repository.
    notrepo = sh("mktemp -d /tmp/snotrepo.XXXXXX")
    serpis.diff.open({ dir = notrepo })
  end,
  function(go)
    when(function() return info().error ~= nil end, go)
    return false
  end,
  function(go)
    T.check("a folder outside git says so", (info().error or ""):find("not in a git repository", 1, true) ~= nil, true)
    return shot("diff-no-repo", go)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then
    sh("rm -rf '" .. repo .. "' '" .. (notrepo or "/nonexistent") .. "'")
    return T.finish()
  end
  if steps[i](go) ~= false then serpis.defer(300, go) end
end
go()
