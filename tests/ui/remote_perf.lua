-- Not part of the suite: frame times of an empty chat moved to another machine with the
-- composer's pill, left idle. A node here (or SERPIS_TEST_REMOTE_HOST over ssh, rayfish on).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end
local box = sh("mktemp -d /tmp/snperf.XXXXXX"):gsub("%s+$", "")
sh("mkdir -p '" .. box .. "/home/proj' '" .. box .. "/node'")
local home = sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
serpis.var.set("remote-offer-setup", false)
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-hosts", { { name = "box", command = { "sh", "-c", "exec env HOME=" .. home .. " SERPIS_AGENT_DIR=" .. home .. "/a SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock " .. root .. "/target/debug/serpis-node agent" } } })
local function stats(n)
  local t = d.frame_times()
  local c, sum, max, slow = 0, 0, 0, 0
  for i = math.max(1, #t - n + 1), #t do c = c + 1; sum = sum + t[i]; if t[i] > max then max = t[i] end; if t[i] > 50 then slow = slow + 1 end end
  return string.format("frames=%d avg=%.1fms max=%.1fms over50ms=%d", c, sum / math.max(c, 1), max, slow)
end
local function window(name, ms, go)
  local f0 = d.frames()
  serpis.defer(ms, function()
    local n = d.frames() - f0
    T.check(string.format("%s: %d frames in %ds, %s", name, n, ms / 1000, stats(math.min(n, 240))), true, true)
    go()
  end)
end
local steps = {
  function(go) serpis.command.run("new-chat"); window("empty local chat, idle", 3000, go) end,
  function(go)
    serpis.command.run("chat-select-machine")
    serpis.defer(300, function()
      T.press("b o x RET")
      local function wait() if (d.minibuffer() or ""):find("Folder", 1, true) then T.press("~ / p r o j RET"); serpis.defer(1500, go) else serpis.defer(100, wait) end end
      wait()
    end)
  end,
  function(go) T.check("moved: " .. tostring(serpis.chat.info().machine), true, true); window("empty remote chat, idle", 3000, go) end,
  function(go) serpis.command.run("remote-stop-node", "box"); serpis.defer(500, function() sh("rm -rf '" .. box .. "'"); go() end) end,
}
local i = 0
local function go() i = i + 1; if steps[i] then steps[i](go) else T.finish() end end
go()
