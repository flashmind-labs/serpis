-- The buffer switcher finds chats and terminals by their project, branch or folder, each typed
-- word within one of them (not spelled from letters of several), and lights up what matched
-- beside the name.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "/alphaproj' '" .. tmp .. "/betaproj' '" .. tmp .. "/gamma notes' && (cd '" .. tmp .. "/alphaproj' && git init -q -b main) && (cd '" .. tmp .. "/betaproj' && git init -q -b dev)")
local alpha = sh("cd '" .. tmp .. "/alphaproj' && pwd -P")
local beta = sh("cd '" .. tmp .. "/betaproj' && pwd -P")
local gamma = sh("cd '" .. tmp .. "/gamma notes' && pwd -P")
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write('{"providers":{"fake":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}')
f:close()
serpis.agent.reload_models()
serpis.agent.model("fake/m")
local function keys(text)
  local out = {}
  for c in text:gmatch(".") do out[#out + 1] = c == " " and "SPC" or c end
  return table.concat(out, " ")
end
-- Labels found, in order, each with what lit up in its detail.
local function found()
  local out = {}
  for _, c in ipairs(d.candidates()) do out[#out + 1] = c.label .. (c.matched_detail ~= "" and (" [" .. c.matched_detail .. "]") or "") end
  return table.concat(out, ", ")
end
local cases = {
  { "alpha", "Terminal [alpha], Fix the loop [alpha]", "by project, lit up beside the name" },
  { "dev", "Terminal 2 [dev], Make tests pass [dev]", "by branch" },
  { "notes", "Terminal 3 [notes]", "by folder (not a project)" },
  { "alpha term", "Terminal [alpha]", "a word is never spelled from letters of several fields" },
  { "beta chat", "Make tests pass [Chatbeta]", "by project and kind" },
  { "loop", "Fix the loop", "by name: nothing lit beside it" },
}
local steps = {
  function()
    serpis.command.run("new-chat", alpha)
    serpis.buffer.set_title(serpis.buffer.current(), "Fix the loop")
    serpis.command.run("new-chat", beta)
    serpis.buffer.set_title(serpis.buffer.current(), "Make tests pass")
    serpis.command.run("new-terminal", alpha)
    return 1200
  end,
  function() serpis.command.run("new-terminal", beta); return 1200 end,
  function() serpis.command.run("new-terminal", gamma); return 1200 end,
}
for _, c in ipairs(cases) do
  steps[#steps + 1] = function() d.key("p", {"cmd"}); return 300 end
  steps[#steps + 1] = function() T.press(keys(c[1])); return 300 end
  steps[#steps + 1] = function()
    T.check(c[3] .. " (" .. c[1] .. ")", found(), c[2])
    T.press("ESC")
    return 300
  end
end
steps[#steps + 1] = function()
  sh("rm -rf '" .. tmp .. "'")
  T.finish()
  return 100000
end
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
