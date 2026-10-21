-- chat-resume opens on this project's sessions; TAB cycles to all projects, then to this folder
-- (a subdirectory of the project here), and back; S-TAB goes the other way. Synthetic sessions.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1"); local out = p:read("*a"); p:close()
  return (out:gsub("%s+$", ""))
end
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "/A/sub' '" .. tmp .. "/B' && cd '" .. tmp .. "/A' && git init -q -b main && cd ../B && git init -q -b main")
local A = sh("cd '" .. tmp .. "/A' && pwd -P")
local B = sh("cd '" .. tmp .. "/B' && pwd -P")
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
local function session(name, cwd, prompt)
  sh("mkdir -p '" .. agent_dir .. "/sessions/x'")
  local f = io.open(agent_dir .. "/sessions/x/" .. name .. ".jsonl", "w")
  f:write(string.format('{"type":"session","version":3,"id":"%s","timestamp":"2026-01-01T00:00:00Z","cwd":"%s"}\n', name, cwd))
  f:write(string.format('{"type":"message","id":"m1","parentId":null,"timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"%s"}],"timestamp":1}}\n', prompt))
  f:close()
end
session("s1", A, "alpha root")
session("s2", A .. "/sub", "alpha sub")
session("s3", B, "beta")
serpis.var.set("session-directories", {})

local function labels()
  local out = {}
  for _, c in ipairs(d.candidates()) do out[#out + 1] = (c.label:match("^(%a+ ?%a*)"):gsub("%s+$", "")) end
  table.sort(out)
  return table.concat(out, ",")
end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    list[i]()
    serpis.defer(300, go)
  end
  go()
end
steps({
  function()
    serpis.command.run("new-chat", A .. "/sub")
    serpis.command.run("session-rescan")
  end,
  function() serpis.defer(1500, function() end) end,
  function() serpis.command.run("chat-resume") end,
  function()
    T.check("opens on this project's sessions", labels(), "alpha root,alpha sub")
    T.check("the prompt names the scope and what TAB does", (d.minibuffer() or ""):find("project A · TAB: all projects", 1, true) ~= nil, true)
    T.press("TAB")
  end,
  function()
    T.check("TAB: all projects", labels(), "alpha root,alpha sub,beta")
    T.press("TAB")
  end,
  function()
    T.check("TAB: this folder", labels(), "alpha sub")
    T.press("TAB")
  end,
  function()
    T.check("TAB: back to the project", labels(), "alpha root,alpha sub")
    T.press("S-TAB")
  end,
  function()
    T.check("S-TAB goes back", labels(), "alpha sub")
    d.text("zzz")
  end,
  function()
    T.press("TAB")
  end,
  function()
    T.check("what was typed is kept", d.composer_state and (d.minibuffer() ~= nil) and #d.candidates(), 0)
    d.key("Escape")
  end,
})
