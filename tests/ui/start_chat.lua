-- Opening the app doesn't add a chat each time: start_chat_restore checks what comes back.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local function chats()
  local n = 0
  for _, b in ipairs(serpis.buffer.list()) do if b.kind == "chat" then n = n + 1 end end
  return n
end
T.check("one chat, on the first start", chats(), 1)
T.check("shown", serpis.buffer.kind(serpis.buffer.current()), "chat")
serpis.command.run("new-chat")
serpis.defer(500, function()
  T.check("a second empty one", chats(), 2)
  T.finish()
end)
