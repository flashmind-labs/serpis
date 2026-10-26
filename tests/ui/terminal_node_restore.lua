-- Runs after terminal_node.lua with the same config and home: the terminals it left shown are
-- shown again, still running, with what they printed; then everything it made is ended.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local out = os.getenv("SERPIS_TEST_OUT"):gsub("terminal_node_restore%.out$", "terminal_node.out")
local f = io.open(out .. ".box"); local box = f and f:read("*a"); if f then f:close() end

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
local function terminals()
  local l = {}
  for _, b in ipairs(serpis.buffer.list()) do
    if b.kind == "terminal" then l[#l + 1] = b.id end
  end
  return l
end
local function on_box()
  for _, id in ipairs(terminals()) do
    if serpis.buffer.machine(id) == "box" then return id end
  end
end
local function has(text) return (serpis.buffer.text() or ""):find(text, 1, true) ~= nil end

when(function() return #terminals() == 2 and on_box() end, function()
  T.check("both terminals are back", #terminals(), 2)
  local b = on_box()
  T.check("the box's among them", b ~= nil, true)
  if not b then return T.finish() end
  serpis.buffer.show(b)
  when(function() return has("left-42-running") end, function()
    T.check("with what it printed", has("left-42-running"), true)
    serpis.buffer.insert('echo "still-$((6*7))"\r')
    when(function() return has("still-42") end, function()
      T.check("and still running", has("still-42"), true)
      -- Ended with their buffers; the box's nodes stopped and its home removed.
      for _, id in ipairs(terminals()) do serpis.buffer.kill(id) end
      serpis.defer(1000, function()
        local env = "HOME=" .. box .. "/home SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock SERPIS_TERMINAL_SOCKET=" .. box .. "/t.sock "
        os.execute("env " .. env .. root .. "/target/debug/serpis-node stop >/dev/null 2>&1; env " .. env .. root .. "/target/debug/serpis-node stop-terminals >/dev/null 2>&1")
        if box and box:match("^/tmp/stbox%.") then os.execute("command rm -rf '" .. box .. "'") end
        T.finish()
      end)
    end)
  end)
end)
