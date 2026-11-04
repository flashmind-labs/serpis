-- Checklist: a short list to go through before starting a task and before shipping it. Copy to
-- ~/.config/serpis/extensions/.
--
-- A chat's header shows how far along it is ("✓ 2/8"), and nothing ever opens by itself:
--
--   a click on that pill, or C-c x (M-x chat-checklist)  the list; click an item, or SPC or RET,
--                               to tick or untick it; j and k move, ESC leaves
--   X on the diff page          the same list, for the chat the review goes to
--   chat-checklist-reset        untick everything, for the next task in the same chat
--
-- An item starting with "Ask:" sends the rest of its line to the chat's agent, ticks it and
-- closes the list, so you can read the answer. Only
-- you tick items: the agent is never told about the list, and serpis blocks nothing.
--
-- The list is Markdown: `## Section` headings and `- [ ] item` lines; anything else is ignored.
-- .serpis/checklist.md at the top of a repository is used in place of checklist-file (default
-- ~/.serpis/checklist.md), and DEFAULT below is used when neither exists. A chat on another
-- machine uses this machine's list. Lists are read when a chat first shows and when you open
-- them. A list with no items hides the pill.
--
-- Settings: checklist-file, checklist-pill. Scripts get the hook checklist-ticked ({buffer, item,
-- done}) and can run chat-checklist with an item's text, as if it was picked there.
--
-- Ticks are kept in the chat's session, so they survive resuming, and never reach the model.

if serpis.where ~= "window" then return end -- The list is for the person, at the window.

local ENTRY = "checklist"

local DEFAULT = [[
## Before starting
- [ ] I wrote down what I expect the answer to be
- [ ] "Done" is something I can point to: a test, a log, a screenshot
- [ ] I said what's out of scope

## Before shipping
- [ ] I read the commands it ran and their output before its summary
- [ ] I know what the passing checks didn't cover
- [ ] Ask: List the decisions and assumptions you made that I didn't specify.
- [ ] Ask: Did I miss anything?
- [ ] I could explain this diff to whoever has to maintain it
]]

serpis.var.define("checklist-file", "~/.serpis/checklist.md", "The checklist used where a repository has no .serpis/checklist.md of its own.")
serpis.var.define("checklist-pill", true, "Show a chat's checklist progress in its header.")

-- The items of a list, in order: {text, section}. An item listed twice counts once, since ticks
-- go by an item's text.
local function parse(text)
  local items, seen, section = {}, {}, ""
  for line in (text .. "\n"):gmatch("(.-)\r?\n") do
    local heading = line:match("^##%s+(.-)%s*$")
    local item = line:match("^%s*[-*]%s+%[[ xX]%]%s+(.-)%s*$")
    if heading then
      section = heading
    elseif item and item ~= "" and not seen[item] then
      seen[item] = true
      table.insert(items, { text = item, section = section })
    end
  end
  return items
end

local function read_file(path)
  local f = io.open(path)
  if not f then return nil end
  local text = f:read("*a")
  f:close()
  return text
end

local function expand(path)
  return (path:gsub("^~/", (os.getenv("HOME") or "~") .. "/"))
end

-- What a chat's list says now, from its files.
local function load_items(buf)
  local info = serpis.chat.info(buf)
  if info and info.cwd and not info.machine then
    local ok, repo = pcall(serpis.project.repo, info.cwd)
    local text = ok and repo and repo.worktree and read_file(repo.worktree .. "/.serpis/checklist.md")
    if text then return parse(text) end
  end
  return parse(read_file(expand(serpis.var.get("checklist-file"))) or DEFAULT)
end

-- Each chat's list, kept: the header asks for it on every frame.
local lists = {}
local function items_of(buf)
  if not lists[buf] then lists[buf] = load_items(buf) end
  return lists[buf]
end

-- The items ticked now, from the session's entries: {text = true}.
local function ticks(buf)
  local set = {}
  for _, e in ipairs(serpis.chat.entries(buf, ENTRY)) do
    if e.reset then
      set = {}
    elseif e.item then
      set[e.item] = e.done or nil
    end
  end
  return set
end

local function progress(buf)
  local set, done, items = ticks(buf), 0, items_of(buf)
  for _, item in ipairs(items) do
    if set[item.text] then done = done + 1 end
  end
  return string.format("%d/%d", done, #items), #items
end

local function mark(buf, item, done)
  serpis.chat.append(buf, ENTRY, { item = item, done = done })
  serpis.hook.run("checklist-ticked", { buffer = buf, item = item, done = done })
end

local function ask_text(item)
  return item:match("^Ask:%s*(.+)$")
end

-- Tick or untick an item, or send an "Ask:" one. True when the question went to the agent.
local function pick(buf, item)
  local done = ticks(buf)[item]
  local ask = ask_text(item)
  if done or not ask then
    mark(buf, item, not done)
    return false
  end
  if serpis.chat.running(buf) then
    -- Sent now it would steer the turn, and the answer wouldn't cover the work still to come.
    serpis.message("The agent is working: ask once its turn ends")
    return false
  end
  serpis.chat.prompt(buf, ask)
  mark(buf, item, true)
  return true
end

local function rows(buf)
  local set, list = ticks(buf), {}
  for _, item in ipairs(items_of(buf)) do
    local ask = ask_text(item.text)
    table.insert(list, {
      label = ask or item.text,
      value = item.text,
      checked = set[item.text] == true,
      section = item.section ~= "" and item.section or nil,
      detail = ask and "asks the agent" or nil,
    })
  end
  return list
end

local function open(buf)
  serpis.minibuffer.read({
    prompt = "Checklist",
    candidates = rows(buf),
    search = false,
    on_toggle = function(item)
      if pick(buf, item) then
        serpis.command.run("keyboard-quit")
      else
        serpis.minibuffer.set_candidates(rows(buf))
      end
    end,
  })
end

-- The chat a command acts on: the current one, or the one the current diff page reviews for.
local function target()
  local buf = serpis.buffer.current()
  local kind = serpis.buffer.kind(buf)
  if kind == "chat" then return buf end
  if kind == "diff" then
    local t = serpis.diff.info().target
    if t then return t end
    serpis.error("This review has no chat yet: pick one with diff-select-target")
    return nil
  end
  serpis.error("The checklist works on a chat or its diff page")
end

serpis.ui.header("checklist", function(buf)
  if buf and serpis.var.get("checklist-pill") and serpis.buffer.kind(buf) == "chat" then
    local text, total = progress(buf)
    if total > 0 then return "✓ " .. text end
  end
end, { command = "chat-checklist" })

serpis.command.define("chat-checklist", function(args)
  local buf = target()
  if not buf then return end
  lists[buf] = load_items(buf) -- It may have been edited, or the chat moved.
  local item = args.params[1]
  if item then
    pick(buf, item)
  elseif #lists[buf] == 0 then
    serpis.message("The checklist has no items")
  else
    open(buf)
  end
end, "Go through this chat's checklist: RET ticks or unticks an item, and an Ask: item goes to the agent (param: an item's text, to do that to it).")

serpis.command.define("chat-checklist-reset", function()
  local buf = target()
  if not buf then return end
  serpis.chat.append(buf, ENTRY, { reset = true })
  serpis.message("Checklist cleared")
end, "Untick this chat's whole checklist, for its next task.")

serpis.hook.add("buffer-killed", function(e)
  lists[e.buffer] = nil
end)

serpis.keymap.set("chat", "C-c x", "chat-checklist")
serpis.keymap.set("diff", "X", "chat-checklist")
