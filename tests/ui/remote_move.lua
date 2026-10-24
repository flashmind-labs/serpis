-- Moving a chat with its history: from here to one machine, from there to another, and back,
-- with the composer's machine pill and real input. At every stop it keeps its conversation and
-- its id, and answers; the machine it left keeps its session, marked as moved. The machines
-- are nodes here in throwaway homes, reached through `serpis-node agent` as ssh would.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end
local function write(path, text) local f = io.open(path, "w"); f:write(text); f:close() end
local models = string.format('{"providers":{"mock":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"k","compat":{"openRouter":true},"models":[{"id":"anthropic/claude-sonnet-4.5"}]}}}', os.getenv("SERPIS_TEST_CACHE_URL"))

-- This machine.
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
write(agent_dir .. "/models.json", models)
serpis.agent.reload_models()
serpis.agent.model("mock/anthropic/claude-sonnet-4.5")
serpis.var.set("session-directory", agent_dir .. "/sessions")
-- The chat's folder: a repository on a branch, with uncommitted work in it.
local here = sh("mktemp -d /tmp/snmove-here.XXXXXX"):gsub("%s+$", "")
here = sh("cd '" .. here .. "' && pwd -P"):gsub("%s+$", "")
local repo = here:match("([^/]+)$")
sh("cd '" .. here .. "' && git init -q -b main && git config user.email t@t && git config user.name t && printf 'v1\\n' > app.txt && git add -A && git commit -qm first && git checkout -qb feature && printf 'v2\\n' > app.txt && printf 'draft\\n' > notes.md")

-- Two other machines, the same model on each.
local boxes, hosts = {}, {}
for _, name in ipairs({ "box1", "box2" }) do
  local b = sh("mktemp -d /tmp/snmove.XXXXXX"):gsub("%s+$", "")
  sh("mkdir -p '" .. b .. "/home/proj' '" .. b .. "/home/.serpis/agent' '" .. b .. "/node'")
  write(b .. "/home/.serpis/agent/models.json", models)
  local home = sh("cd '" .. b .. "/home' && pwd -P"):gsub("%s+$", "")
  boxes[name] = { dir = b, home = home }
  hosts[#hosts + 1] = { name = name, command = { "sh", "-c", "exec env HOME=" .. home .. " SERPIS_AGENT_DIR=" .. home .. "/.serpis/agent SERPIS_NODE_DIR=" .. b .. "/node SERPIS_NODE_SOCKET=" .. b .. "/node/node.sock " .. root .. "/target/debug/serpis-node agent" } }
end
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-offer-setup", false)
serpis.var.set("remote-hosts", hosts)

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    local wait = list[i](go)
    if wait ~= false then serpis.defer(wait or 300, go) end
  end
  go()
end
local function when(ready, f, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local function prompt() return d.minibuffer() or "" end
local function click(name)
  local r = d.composer()[name]
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
end
local chat, id
local function transcript() return serpis.chat.transcript(chat) end
-- Lines that are exactly `text` ("ok" is the mock's answer; "tokens" is not one).
local function count(text) local n = 0; for l in (transcript() .. "\n"):gmatch("([^\n]*)\n") do if l:match("^%s*(.-)%s*$") == text then n = n + 1 end end; return n end
local function idle() return not serpis.chat.running(chat) end
-- Whether a message since the last `mark()` says `text`.
local seen = 0
local function mark() seen = #d.messages() end
local function said(text) local ms = d.messages(); for i = seen + 1, #ms do if ms[i]:find(text, 1, true) then return true end end end
-- Ask `text` and wait for the answer ("ok" from the mock).
local function ask(text, answers)
  return function(go)
    T.press(text:gsub(".", "%0 "):gsub("  ", " SPC ") .. "RET")
    when(function() return count("ok") >= answers and idle() end, go)
    return false
  end
end
local function keys(text) return (text:gsub(".", "%0 "):gsub("   ", " SPC ")) end
-- Move the chat with the pill to `machine`: bringing its folder (`folder` nil), or into
-- `folder` there.
local function move(machine, folder, wait_for)
  local list = {
    function(go)
      mark()
      click("machine")
      when(function() return prompt():find("Move this chat to", 1, true) ~= nil end, go)
      return false
    end,
    function(go)
      T.press(keys(machine) .. "RET")
      when(function() return prompt():find("Its folder", 1, true) ~= nil end, go)
      return false
    end,
  }
  if folder then
    list[#list + 1] = function(go)
      T.press("u s e RET")
      when(function() return prompt():find("Folder on", 1, true) ~= nil or prompt():find("Continue it in", 1, true) ~= nil end, go)
      return false
    end
    list[#list + 1] = function(go)
      T.press(keys(folder) .. "RET")
      when(function() return said("The chat continues on " .. wait_for) end, go)
      return false
    end
  else
    list[#list + 1] = function(go)
      T.press("b r i n g RET")
      when(function() return said("The chat continues on " .. wait_for) end, go)
      return false
    end
  end
  return list
end
local function git_state(dir) return sh("cd '" .. dir .. "' && git branch --show-current && git status --porcelain && cat app.txt") end
local function sessions_of(dir) return sh("cat '" .. dir .. "'/*/*.jsonl 2>/dev/null") end

local list = {
  function()
    serpis.command.run("new-chat", here)
    chat = serpis.buffer.current()
  end,
  ask("hello", 1),
  function()
    id = serpis.chat.info(chat).session_id
    T.check("it ran here", serpis.chat.info(chat).machine, nil)
  end,
}
local function add(t) for _, s in ipairs(t) do list[#list + 1] = s end end
add(move("box1", nil, "box1"))
add({
  function()
    local info = serpis.chat.info(chat)
    T.check("here -> box1: it runs there", info.machine, "box1")
    T.check("in its folder, brought along to serpis's place for it", info.cwd, boxes.box1.home .. "/.serpis/worktrees/" .. repo .. "/feature")
    T.check("on its branch, with its work uncommitted", git_state(info.cwd), "feature\n M app.txt\n?? notes.md\nv2\n")
    T.check("as the same chat", info.session_id, id)
    T.check("with its conversation", count("hello") >= 1 and count("ok") == 1, true)
    T.check("this machine kept its session, marked as moved", sessions_of(agent_dir .. "/sessions"):find('"serpis-moved"', 1, true) ~= nil, true)
  end,
  ask("next", 2),
  function() T.check("and it answers there", count("ok"), 2) end,
})
add(move("box2", nil, "box2"))
add({
  function()
    local info = serpis.chat.info(chat)
    T.check("box1 -> box2", info.machine, "box2")
    T.check("its folder brought from one machine to another", info.cwd, boxes.box2.home .. "/.serpis/worktrees/" .. repo .. "/feature")
    T.check("as it was", git_state(info.cwd), "feature\n M app.txt\n?? notes.md\nv2\n")
    T.check("still the same chat", info.session_id, id)
    T.check("with all of it", count("hello") >= 1 and count("next") >= 1 and count("ok") == 2, true)
    T.check("box1 kept its session, marked as moved", sessions_of(boxes.box1.home .. "/.serpis/agent/sessions"):find('"serpis-moved"', 1, true) ~= nil, true)
  end,
  ask("again", 3),
})
add(move("local", here, "this machine"))
add({
  function()
    local info = serpis.chat.info(chat)
    T.check("box2 -> here", info.machine, nil)
    T.check("in the folder picked here", info.cwd, here)
    T.check("the same chat, back", info.session_id, id)
    T.check("with everything said on three machines", count("ok") == 3 and count("again") >= 1, true)
  end,
  ask("home", 4),
  function() T.check("and it answers here again", count("ok"), 4) end,
})
-- To box1 again: the same folder there, brought up to date, not a copy.
add(move("box1", nil, "box1"))
add({
  function()
    T.check("box1 again: its folder there, not a copy", serpis.chat.info(chat).cwd, boxes.box1.home .. "/.serpis/worktrees/" .. repo .. "/feature")
    T.check("no second copy", sh("ls '" .. boxes.box1.home .. "/.serpis/worktrees/" .. repo .. "'"), "feature\n")
    T.check("the message says where", said("in " .. boxes.box1.home .. "/.serpis/worktrees/" .. repo .. "/feature") == true, true)
    serpis.command.run("remote-stop-node", "box1")
    serpis.command.run("remote-stop-node", "box2")
  end,
  function(go)
    when(function() return sh("ls '" .. boxes.box1.dir .. "/node' '" .. boxes.box2.dir .. "/node'"):find("node.sock", 1, true) == nil end, function()
      sh("rm -rf '" .. boxes.box1.dir .. "' '" .. boxes.box2.dir .. "' '" .. here .. "'")
      go()
    end)
    return false
  end,
})
steps(list)
