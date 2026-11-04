-- Runs after start_chat with the same config: its empty chats closed with the window.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local function chats()
  local n = 0
  for _, b in ipairs(serpis.buffer.list()) do if b.kind == "chat" then n = n + 1 end end
  return n
end
serpis.defer(1000, function()
  T.check("still one chat, after starting again", chats(), 1)
  T.check("shown", serpis.buffer.kind(serpis.buffer.current()), "chat")
  T.check("and it is alive", serpis.chat.info().session_id ~= nil, true)
  T.finish()
end)
