-- A new serpis, over what diff_resume left: the review carries on where it was.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local function sh(cmd) local p = io.popen(cmd .. " 2>&1"); local o = p:read("*a"); p:close(); return (o:gsub("%s+$", "")) end
local repo = sh("cd \"$HOME\" && pwd -P") .. "/proj"
local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(250, go)
end
local function info() return serpis.diff.info() end
local steps = {
  function(go)
    serpis.diff.open({ dir = repo })
    when(function() return serpis.buffer.current() and serpis.buffer.kind():find("diff", 1, true) and info().rows > 0 and info().writing ~= nil end, go)
    return false
  end,
  function(go)
    T.check("what was compared", info().compared, "Unstaged changes")
    T.check("the file that was showing", info().file, "c.txt")
    local c = serpis.diff.comments()
    T.check("only what wasn't sent", #c, 1)
    T.check("the comment waiting", c[1] and c[1].text, "this one waits")
    T.check("the half written one, open", info().writing and info().writing.text, "half writ")
    T.check("still a question", info().writing and info().writing.kind, "question")
    d.text("ten")
    serpis.defer(250, function()
      T.press("C-RET")
      when(function() return info().writing == nil end, go)
    end)
    return false
  end,
  function()
    T.check("typing goes on, and it is kept", serpis.diff.comments()[2].text, "half written")
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then d.screenshot(shots .. "/diff-resumed.png") end
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(600, go) end
end
go()
