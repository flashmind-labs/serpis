-- Not part of the suite: frame times with chats on another machine in view. By default a node
-- here; with SERPIS_TEST_REMOTE_HOST a real machine (as remote.lua), rayfish discovery on.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end
local remote = os.getenv("SERPIS_TEST_REMOTE_HOST")
local function q(s) return "'" .. s:gsub("'", "'\\''") .. "'" end
local function far(cmd)
  if remote then return sh("ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new " .. remote .. " -- " .. q("sh -c " .. q(cmd))) end
  return sh(cmd)
end
local box = remote and ("/tmp/snperf-" .. os.time()) or sh("mktemp -d /tmp/snperf.XXXXXX"):gsub("%s+$", "")
far("mkdir -p " .. box .. "/home/proj " .. box .. "/agent " .. box .. "/node")
local mock_url, mock_pid = os.getenv("SERPIS_TEST_CACHE_URL"), nil
if remote then
  local out = far("nohup " .. os.getenv("SERPIS_TEST_REMOTE_MOCK") .. " " .. box .. "/mock.port " .. box .. "/mock.log >/dev/null 2>&1 & echo $!; for i in $(seq 50); do [ -s " .. box .. "/mock.port ] && break; sleep 0.1; done; cat " .. box .. "/mock.port")
  local pid, port = out:match("(%d+)%s+(%d+)")
  mock_pid, mock_url = pid, "http://127.0.0.1:" .. port
end
far("printf '%s' " .. q(string.format('{"providers":{"boxai":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"x","compat":{"openRouter":true},"models":[{"id":"anthropic/claude-sonnet-4.5","input":["text","image"]}]}}}', mock_url)) .. " > " .. box .. "/agent/models.json")
local home = remote and (box .. "/home") or sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
local env = { HOME = home, SERPIS_NODE_DIR = box .. "/node", SERPIS_NODE_SOCKET = box .. "/node/node.sock", SERPIS_AGENT_DIR = box .. "/agent" }
if remote then
  serpis.var.set("remote-ssh-args", { "-o", "StrictHostKeyChecking=accept-new" })
  serpis.var.set("remote-node-builds", os.getenv("SERPIS_TEST_NODE_BUILDS"))
  serpis.var.set("remote-hosts", { { name = "box", host = remote, env = env } })
else
  serpis.var.set("remote-rayfish", false)
  local e = ""
  for k, v in pairs(env) do e = e .. k .. "=" .. v .. " " end
  serpis.var.set("remote-hosts", { { name = "box", command = { "sh", "-c", "exec env " .. e .. root .. "/target/debug/serpis-node agent" } } })
end
-- A photo-sized image (about 1 MB of PNG).
local img = box .. ".png"
sh("python3 -c 'import zlib,struct,random\nw=h=600\nraw=b\"\".join(b\"\\x00\"+bytes(random.getrandbits(8) for _ in range(w*3)) for _ in range(h))\nc=lambda t,d:struct.pack(\">I\",len(d))+t+d+struct.pack(\">I\",zlib.crc32(t+d))\nopen(\"" .. img .. "\",\"wb\").write(b\"\\x89PNG\\r\\n\\x1a\\n\"+c(b\"IHDR\",struct.pack(\">IIBBBBB\",w,h,8,2,0,0,0))+c(b\"IDAT\",zlib.compress(raw))+c(b\"IEND\",b\"\"))'")

local function stats(from)
  local t = d.frame_times()
  local n, sum, max, slow = 0, 0, 0, 0
  for i = math.max(1, #t - from + 1), #t do n = n + 1; sum = sum + t[i]; if t[i] > max then max = t[i] end; if t[i] > 50 then slow = slow + 1 end end
  return string.format("frames=%d avg=%.1fms max=%.1fms over50ms=%d", n, sum / math.max(n, 1), max, slow)
end
local chat
local function phase(name, wait, f)
  return function(go)
    local before = d.frames()
    f()
    serpis.defer(wait, function()
      T.check(name .. ": " .. stats(math.min(240, d.frames() - before)), true, true)
      go()
    end)
    return false
  end
end
local i, list = 0, nil
local function go() i = i + 1; if list[i] then local w = list[i](go); if w ~= false then serpis.defer(w or 300, go) end else T.finish() end end
list = {
  phase("local chat, idle", 2000, function() serpis.command.run("new-chat") end),
  phase("connect and open a remote chat", 8000, function() serpis.command.run("remote-new-chat", "box", "~/proj"); end),
  function() chat = serpis.buffer.current() end,
  phase("remote chat, idle", 3000, function() end),
  phase("remote chat with an image, answering", 4000, function() serpis.chat.attach(chat, img); serpis.chat.prompt(chat, "hello") end),
  phase("remote chat with an image, idle", 3000, function() end),
  function()
    serpis.command.run("remote-stop-node", "box")
    serpis.defer(800, function() far((mock_pid and ("kill " .. mock_pid .. "; ") or "") .. "rm -rf " .. box); sh("rm -f " .. img) end)
  end,
}
go()
