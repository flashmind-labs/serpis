-- Runs after sessions.lua with the same config: the chat it left open is back, unshown.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local out = os.getenv("SERPIS_TEST_OUT"):gsub("sessions_restore%.out$", "sessions.out")
local f = io.open(out .. ".path")
local want = f and f:read("*a")
if f then f:close() end
local found
for _, b in ipairs(serpis.buffer.list()) do
  if serpis.session.of(b.id) == want then found = b.id end
end
T.check("left-open chat restored", found ~= nil, true)
T.check("restored chat keeps its title", found and serpis.buffer.title(found), "Greeting")
T.check("restored chat is not shown", serpis.buffer.current() ~= found, true)
os.execute("rm -rf '" .. out .. ".d'")
T.finish()
