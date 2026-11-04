-- The diff page's warning that the local base branch is out of date with its remote: a throwaway
-- origin on disk that moves on, then a fetch, then a pull.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local function sh(cmd) local p = io.popen(cmd .. " 2>&1"); local o = p:read("*a"); p:close(); return (o:gsub("%s+$", "")) end
local top = sh("mktemp -d /tmp/supstream.XXXXXX")
top = sh("cd '" .. top .. "' && pwd -P")
local here, there = top .. "/here", top .. "/there"
local id = "git config user.email t@t && git config user.name t"
sh("cd '" .. top .. "' && git init -q --bare -b main origin.git && git clone -q origin.git here && cd here && " .. id
  .. " && git checkout -q -b main && echo one > a.txt && git add a.txt && git commit -qm one && git push -q -u origin main && git checkout -q -b work && echo two > a.txt")
sh("cd '" .. top .. "' && git clone -q origin.git there && cd there && " .. id)
local function up() return serpis.diff.info().upstream end

local i, list = 0, nil
local function go()
  i = i + 1
  local s = list[i]
  if not s then
    sh("rm -rf '" .. top .. "'")
    return T.finish()
  end
  local wait = s(go)
  if wait ~= false then serpis.defer(wait or 800, go) end
end
list = {
  function()
    serpis.var.set("diff-refresh-seconds", 0.2)
    serpis.var.set("diff-upstream-check-seconds", 0.5)
    serpis.diff.open({ dir = here })
    return 1500
  end,
  function()
    T.check("up to date: no warning", up() and up().warning, nil)
    T.check("it did ask", up() and up().upstream, "origin/main")
    T.check("no icon", d.rect("diff-upstream-warning"), nil)
    sh("cd '" .. there .. "' && echo three > b.txt && git add b.txt && git commit -qm three && git push -q origin main")
    return 1500
  end,
  function()
    T.check("the remote moved: not fetched", up().unfetched, true)
    T.check("the warning says to fetch", (up().warning or ""):find("Fetch or pull", 1, true) ~= nil, true)
    T.check("the icon shows", d.rect("diff-upstream-warning") ~= nil, true)
    T.check("asking fetched nothing", sh("cd '" .. here .. "' && git cat-file -e " .. up().tip .. "^{commit} 2>/dev/null && echo known || echo unknown"), "unknown")
    T.check("the page still compares with local main", serpis.diff.info().compared, "main → worktree")
    d.screenshot(os.getenv("SERPIS_TEST_OUT") .. ".warning.png")
    sh("cd '" .. here .. "' && git fetch -q origin")
    return 1000
  end,
  function()
    T.check("fetched: behind by one", up().behind, 1)
    T.check("and says so", (up().warning or ""):find("Local main is 1 commit behind origin/main", 1, true) ~= nil, true)
    -- Off the remote's clock: only the look at the answer can clear it.
    serpis.var.set("diff-upstream-check-seconds", 3600)
    return 800
  end,
  function()
    sh("cd '" .. here .. "' && git fetch -q origin main:main")
    return 1000
  end,
  function()
    T.check("pulled: the warning clears by itself", up().warning, nil)
    T.check("the icon is gone", d.rect("diff-upstream-warning"), nil)
    serpis.var.set("diff-upstream-check-seconds", 0)
    return 500
  end,
  function()
    T.check("0 turns it off", up(), nil)
  end,
}
go()
