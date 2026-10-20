-- Max Money: stop a chat once its session has spent more than a dollar limit (a port of the
-- pi extension). Copy to ~/.config/serpis/extensions/.
--
--   M-x max-money        asks for the limit: dollars, "off", or empty to show what was spent
--   max-money 5          (from Lua: serpis.command.run("max-money", 5)) stop past $5
--   max-money off        remove the limit
--
-- Cost is the whole session's spend. Once over the limit, the current run is aborted, tool
-- calls are blocked and new prompts are refused until a higher limit is set. The check runs
-- after each model response, so the response that crosses the limit is still paid for.
-- The limit is saved in the session, so it survives resuming.

local ENTRY = "max-money"

local function fmt(n)
  return string.format("$%.2f", n)
end

-- The latest limit recorded in the chat's session (nil: none, or turned off).
local function limit(buf)
  local l
  for _, e in ipairs(serpis.chat.entries(buf, ENTRY)) do
    l = e.limit
  end
  return l
end

local function spent(buf)
  return serpis.chat.usage(buf).cost
end

local function over(buf)
  local l = limit(buf)
  return l ~= nil and spent(buf) > l
end

local function stopped(buf)
  return string.format("Spent %s, over the %s limit. Set a higher one with max-money to carry on.", fmt(spent(buf)), fmt(limit(buf)))
end

-- "$1.20/$5.00" in the header of chats that have a limit.
serpis.ui.header("max-money", function(buf)
  if buf and serpis.buffer.kind(buf) == "chat" then
    local l = limit(buf)
    if l then
      return fmt(spent(buf)) .. "/" .. fmt(l)
    end
  end
end)

serpis.hook.add("assistant-message", function(e)
  if e.buffer and over(e.buffer) and serpis.chat.running(e.buffer) then
    serpis.chat.abort(e.buffer)
    serpis.error(stopped(e.buffer))
  end
end)

serpis.hook.add("before-tool-call", function(e)
  if e.buffer and over(e.buffer) then
    return { block = "Session spending limit reached" }
  end
end)

serpis.hook.add("before-prompt", function(e)
  if e.buffer and over(e.buffer) then
    return { block = stopped(e.buffer) }
  end
end)

local function apply(buf, arg)
  arg = tostring(arg or ""):gsub("^%s+", ""):gsub("%s+$", ""):gsub("^%$", "")
  if arg == "" then
    local l = limit(buf)
    serpis.message(l and string.format("Spent %s of %s.", fmt(spent(buf)), fmt(l)) or string.format("No limit set. Spent %s.", fmt(spent(buf))))
    return
  end
  local new
  if arg ~= "off" then
    new = tonumber(arg)
    if not new or new <= 0 or new ~= new or new == math.huge then
      serpis.error("Usage: max-money <dollars> | off")
      return
    end
  end
  serpis.chat.append(buf, ENTRY, { limit = new })
  if new == nil then
    serpis.message("Spending limit removed")
  elseif spent(buf) > new then
    serpis.message(string.format("%s is already below the %s spent", fmt(new), fmt(spent(buf))))
  else
    serpis.message(string.format("Will stop once the session spends more than %s", fmt(new)))
  end
end

serpis.command.define("max-money", function(args)
  local buf = serpis.buffer.current()
  if serpis.buffer.kind(buf) ~= "chat" then
    serpis.error("max-money works on a chat")
    return
  end
  if args.params[1] ~= nil then
    apply(buf, args.params[1])
    return
  end
  local l = limit(buf)
  serpis.minibuffer.read({
    prompt = string.format("Max money (spent %s; dollars or off): ", fmt(spent(buf))),
    initial = l and tostring(l) or "",
    on_accept = function(r)
      apply(buf, r.input)
    end,
  })
end, "Stop this chat once its session has spent more than N dollars (param: dollars or \"off\").")
