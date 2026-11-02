-- Starting a review and quitting: this half leaves comments, one of them sent and one half
-- written, for diff_resume_restore to find in a new serpis.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local function sh(cmd) local p = io.popen(cmd .. " 2>&1"); local o = p:read("*a"); p:close(); return (o:gsub("%s+$", "")) end
local function write(path, text) local f = io.open(path, "w"); f:write(text); f:close() end
local home = sh("cd \"$HOME\" && pwd -P")
local repo = home .. "/proj"
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "' '" .. repo .. "'")
write(agent_dir .. "/models.json", string.format('{"providers":{"mock":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"k","compat":{"openRouter":true},"models":[{"id":"anthropic/claude-sonnet-4.5","reasoning":false,"input":["text"],"cost":{"input":1,"output":1,"cacheRead":0.1,"cacheWrite":1},"contextWindow":200000,"maxTokens":4096}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
serpis.agent.reload_models()
serpis.agent.model("mock/anthropic/claude-sonnet-4.5")
serpis.var.set("session-directory", agent_dir .. "/sessions")
local function in_repo(cmd) return sh("cd '" .. repo .. "' && " .. cmd) end
write(repo .. "/a.txt", "one\ntwo\nthree\n")
write(repo .. "/b.txt", "uno\ndos\ntres\n")
write(repo .. "/c.txt", "eins\nzwei\ndrei\n")
in_repo("git init -q -b main && git add . && git -c user.email=t@t -c user.name=t commit -qm init")
write(repo .. "/a.txt", "one\nTWO\nthree\n")
write(repo .. "/b.txt", "uno\nDOS\ntres\n")
write(repo .. "/c.txt", "eins\nZWEI\ndrei\n")
in_repo("git add a.txt")

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(250, go)
end
local function info() return serpis.diff.info() end
local function press_when(keys, ready, go)
  T.press(keys)
  when(ready, go)
  return false
end
local chat
local steps = {
  function(go)
    chat = serpis.buffer.create("chat", "*c*", { cwd = repo })
    serpis.buffer.show(chat)
    serpis.defer(300, function()
      T.press("s-D")
      when(function() return serpis.buffer.kind() == "diff" and info().rows > 0 end, go)
    end)
    return false
  end,
  -- This comparison isn't the one a new page would choose.
  function(go)
    T.check("starts on uncommitted", info().compared, "Uncommitted changes")
    serpis.command.run("diff-select-source", "unstaged")
    when(function() return info().compared == "Unstaged changes" and info().file == "b.txt" and info().rows > 0 end, go)
    return false
  end,
  function(go) T.press("g g"); T.press("n"); return press_when("c", function() return info().writing ~= nil end, go) end,
  function() d.text("this one gets sent"); T.press("C-RET") end,
  function(go) return press_when("S", function() return info().drafts == 0 end, go) end,
  function(go) T.press("g g"); T.press("n"); return press_when("c", function() return info().writing ~= nil end, go) end,
  function() d.text("this one waits"); T.press("C-RET") end,
  -- Going to another file keeps what was being written, on the file it is about.
  function(go) return press_when("c", function() return info().writing ~= nil end, go) end,
  function() d.text("kept when leaving the file") end,
  function(go)
    serpis.command.run("diff-select-file", "c.txt")
    when(function() return info().writing == nil and #serpis.diff.comments() == 3 and info().file == "c.txt" and info().rows > 0 end, go)
    return false
  end,
  function()
    T.check("leaving the file keeps the comment", serpis.diff.comments()[3].path .. ": " .. serpis.diff.comments()[3].text, "b.txt: kept when leaving the file")
    serpis.command.run("diff-comment-delete", serpis.diff.comments()[3].id)
  end,
  function(go) T.press("g g"); T.press("n"); return press_when("a", function() return info().writing ~= nil end, go) end,
  function() d.text("half writ") end,
  function()
    T.check("two kept, one half written", info().writing and info().writing.text, "half writ")
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(300, go) end
end
go()
