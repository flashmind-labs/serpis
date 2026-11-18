-- Code comments: whether a chat's agent may write comments in code. Copy to
-- ~/.config/serpis/extensions/.
--
-- Chats start with comments off: the agent is told not to write any, and to mark a comment its
-- change makes wrong instead of fixing it (code-comments-rule). Turn them on or off for one chat:
--
--   C-c ; (M-x code-comments-toggle)   on or off for this chat, from its next message
--   a click on the header's pill        the same ("no comments" shows while they're off)
--
-- Settings: code-comments (on for new chats; off by default), code-comments-rule (what the
-- agent is told while they're off), code-comments-allow (what it's told when they're turned
-- back on), code-comments-pill.
--
-- The rule reaches the agent with the next message you send, as context before it (the
-- transcript shows your message as typed), and again only when the choice changes or the chat
-- compacts its history. The system prompt is never changed, so the prompt cache holds. The
-- choice is kept in the chat's session, so it survives resuming.

local CHOICE = "code-comments"     -- {allowed}: the person's choice for a chat.
local TOLD = "code-comments-told"  -- {allowed} or {reset}: what its agent was last told.

serpis.var.define("code-comments", false, "Let a new chat's agent write comments in code (code-comments-toggle changes it for one chat).")
serpis.var.define("code-comments-rule", [[
Never write any comments in the code.
The human will review the code and write any comments themselves.
If you change any code that invalidates an existing comment, then
prefix the comment with
`// TODO-UPDATE-COMMENT: Reason\n// ---`.]], "What a chat's agent is told while code comments are off.")
serpis.var.define("code-comments-allow", "You may write comments in code again: the earlier rule against writing comments no longer applies.", "What a chat's agent is told when code comments are turned back on.")
serpis.var.define("code-comments-pill", true, "Show \"no comments\" in a chat's header while code comments are off.")

local function last(buf, kind)
  local e = serpis.chat.entries(buf, kind)
  return e[#e]
end

local function allowed(buf)
  local c = last(buf, CHOICE)
  if c then return c.allowed == true end
  return serpis.var.get("code-comments") == true
end

-- In the chat's node: tell the agent when what it was last told isn't the choice now.
serpis.hook.add("before-prompt", function(e)
  local info = serpis.chat.info(e.buffer)
  if info and info.compacting then return end -- Refused: nothing is sent.
  local want, told = allowed(e.buffer), last(e.buffer, TOLD)
  if told and not told.reset and told.allowed == want then return end
  serpis.chat.append(e.buffer, TOLD, { allowed = want })
  if not want then return { context = serpis.var.get("code-comments-rule") } end
  -- Turned on: only an agent told not to comment needs to hear it.
  if told and told.allowed == false then return { context = serpis.var.get("code-comments-allow") } end
end)

-- A summary may drop the message the rule came with: say it again with the next one.
serpis.hook.add("chat-compacted", function(e)
  serpis.chat.append(e.buffer, TOLD, { reset = true })
end)

if serpis.where ~= "window" then return end

serpis.command.define("code-comments-toggle", function()
  local buf = serpis.buffer.current()
  if serpis.buffer.kind(buf) ~= "chat" then return serpis.error("Not a chat") end
  local now = not allowed(buf)
  serpis.chat.append(buf, CHOICE, { allowed = now })
  serpis.message(now and "Code comments allowed in this chat, from the next message"
    or "No code comments in this chat, from the next message")
end, "Let the current chat's agent write comments in code, or stop it.")

serpis.keymap.set("chat", "C-c ;", "code-comments-toggle")

serpis.ui.header("code-comments", function(buf)
  if buf and serpis.var.get("code-comments-pill") and serpis.buffer.kind(buf) == "chat" and not allowed(buf) then
    return "no comments"
  end
end, { command = "code-comments-toggle" })
