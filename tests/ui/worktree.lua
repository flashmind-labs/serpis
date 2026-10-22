-- New worktrees start from a branch the user picks: local branches (the current one first) and
-- remote ones, newest first, or any commit typed in. Then the new branch is named (a remote
-- base suggests its own name and becomes the upstream). Driven with real keys.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
local git = "git -c user.email=t@t -c user.name=t"
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "/proj' '" .. tmp .. "/origin'")
sh("cd '" .. tmp .. "/origin' && git init -q --bare")
sh("cd '" .. tmp .. "/proj' && git init -q -b main && " .. git .. " commit -q --allow-empty -m init"
  .. " && git checkout -q -b feature-a && " .. git .. " commit -q --allow-empty -m 'feature a work'"
  .. " && git checkout -q -b feature-b main && " .. git .. " commit -q --allow-empty -m 'remote work'"
  .. " && git remote add origin '" .. tmp .. "/origin' && git push -q origin feature-b && git checkout -q main"
  .. " && git branch -q -D feature-b && git fetch -q origin && sleep 1 && " .. git .. " commit -q --allow-empty -m 'main work'")
local proj = sh("cd '" .. tmp .. "/proj' && pwd -P")
serpis.var.set("worktree-directory", tmp .. "/wt")

-- The worktree checked out on `branch`, from git itself.
local function worktree_of(branch)
  local path
  for line in sh("git -C '" .. proj .. "' worktree list --porcelain"):gmatch("[^\n]+") do
    local p = line:match("^worktree (.+)")
    if p then path = p end
    if line == "branch refs/heads/" .. branch then return path end
  end
end
local function last_message(prefix)
  local ms = d.messages()
  for i = #ms, 1, -1 do
    if ms[i]:find(prefix, 1, true) then return ms[i] end
  end
end
local function when(ready, f, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local s = list[i]
    if not s then return T.finish() end
    local wait = s(go)
    if wait ~= false then serpis.defer(wait or 300, go) end
  end
  go()
end

local suggested
steps({
  function()
    serpis.command.run("worktree-new-chat", proj)
  end,
  function()
    T.check("it asks what the worktree starts from", d.minibuffer(), "New worktree from: ")
    local c = d.candidates()
    local by = {}
    for i, x in ipairs(c) do by[x.label] = { i = i, detail = x.detail } end
    T.check("the current branch comes first", c[1] and c[1].label, "main")
    T.check("marked as current", c[1] and c[1].detail:find("current", 1, true) ~= nil, true)
    T.check("local branches show their last commit", by["feature-a"] and by["feature-a"].detail:find("feature a work", 1, true) ~= nil, true)
    T.check("remote branches are listed after local ones", by["origin/feature-b"] and by["feature-a"] and by["origin/feature-b"].i > by["feature-a"].i, true)
    T.check("marked as remote", by["origin/feature-b"] and by["origin/feature-b"].detail:find("remote", 1, true) ~= nil, true)
    if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/worktree-base.png") end
    d.text("feature-a")
  end,
  function() d.key("Enter") end,
  function()
    T.check("then it asks for the new branch", d.minibuffer(), "New branch from feature-a: ")
    suggested = d.minibuffer_input()
    T.check("suggesting a name", suggested and suggested:match("^serpis/%w+$") ~= nil, true)
    d.key("Enter")
  end,
  function(go)
    when(function() return worktree_of(suggested) ~= nil end, go)
    return false
  end,
  function()
    local path = worktree_of(suggested)
    T.check("the worktree starts at the picked branch", path and sh("git -C '" .. path .. "' log -1 --format=%s"), "feature a work")
    local info = serpis.chat.info(serpis.buffer.current())
    T.check("and a chat opens in it", info and info.cwd == path, true)
    -- A remote branch: its name is suggested, and it becomes the upstream.
    serpis.command.run("worktree-new-chat", proj)
  end,
  function()
    d.text("origin/feature-b")
  end,
  function() d.key("Enter") end,
  function()
    T.check("a remote base", d.minibuffer(), "New branch from origin/feature-b: ")
    T.check("suggests the remote branch's name", d.minibuffer_input(), "feature-b")
    d.key("Enter")
  end,
  function(go)
    when(function() return worktree_of("feature-b") ~= nil end, go)
    return false
  end,
  function()
    local path = worktree_of("feature-b")
    T.check("the worktree has the remote branch's commit", path and sh("git -C '" .. path .. "' log -1 --format=%s"), "remote work")
    T.check("and tracks it", path and sh("git -C '" .. path .. "' rev-parse --abbrev-ref @{u}"), "origin/feature-b")
    -- Scripted: directory, branch, base.
    serpis.command.run("worktree-new-terminal", proj, "scripted", "feature-a")
  end,
  function(go)
    when(function() return worktree_of("scripted") ~= nil end, go)
    return false
  end,
  function()
    local path = worktree_of("scripted")
    T.check("params pick the base", path and sh("git -C '" .. path .. "' log -1 --format=%s"), "feature a work")
    T.check("worktree-new-terminal opens a terminal", serpis.buffer.kind(serpis.buffer.current()), "terminal")
    -- Something that isn't a branch or commit.
    serpis.command.run("worktree-new-chat", proj)
  end,
  function() d.text("nope") end,
  function() d.key("Enter") end,
  function()
    T.check("an unknown base is refused", last_message("nope is not a branch or commit here") ~= nil, true)
    T.check("without asking further", d.minibuffer(), nil)
    -- The picker can be turned off: new worktrees start from the current checkout.
    serpis.var.set("worktree-pick-base", false)
    serpis.command.run("worktree-new-chat", proj)
  end,
  function()
    T.check("worktree-pick-base off: straight to the branch name", d.minibuffer(), "New worktree on branch: ")
    d.key("Escape")
  end,
})
