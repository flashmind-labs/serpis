-- New chats and terminals start where you are; a chat's header shows its folder, and clicking it
-- changes it; the buffer switcher finds buffers by their project. folders_restore runs next.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "/repo/sub/deep' '" .. tmp .. "/other dir' && cd '" .. tmp .. "/repo' && git init -q -b main")
local repo = sh("cd '" .. tmp .. "/repo' && pwd -P")
local other = sh("cd '" .. tmp .. "/other dir' && pwd -P")
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write('{"providers":{"fake":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"x","models":[{"id":"m"},{"id":"m2","reasoning":true}]}}}')
f:close()
serpis.agent.reload_models()
serpis.agent.model("fake/m")
serpis.var.set("session-directories", {})

local function dir_of(id)
  for _, it in ipairs(serpis.sidebar.items()) do
    if it.buffer == id then return it.dir end
  end
end
local function here() return dir_of(serpis.buffer.current()) end
local function kind() return serpis.buffer.kind(serpis.buffer.current()) end
local function labels()
  local out = {}
  for _, c in ipairs(d.candidates()) do out[c.label] = c.detail end
  return out
end
local function keys(text)
  local out = {}
  for c in text:gmatch(".") do out[#out + 1] = c == " " and "SPC" or c end
  return table.concat(out, " ")
end
local function click(r)
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end

local steps = {
  function()
    T.check("the app starts on a chat", kind(), "chat")
    serpis.command.run("new-terminal", repo .. "/sub")
    return 1500
  end,
  function()
    T.check("a terminal", kind(), "terminal")
    T.press(keys("cd deep") .. " RET")
    return 1000
  end,
  function()
    T.check("the terminal says where it went", here(), repo .. "/sub/deep")
    d.key("t", {"cmd"})
    return 1500
  end,
  function()
    T.check("new terminal from a terminal: in its folder, not the project's", here(), repo .. "/sub/deep")
    T.check("(it is a new terminal)", kind(), "terminal")
    d.key("n", {"cmd"})
    return 800
  end,
  function()
    T.check("new chat from a terminal", kind(), "chat")
    T.check("in its folder", here(), repo .. "/sub/deep")
    T.check("the node agrees", serpis.chat.info().cwd, repo .. "/sub/deep")
    d.key("n", {"cmd"})
    return 800
  end,
  function()
    T.check("new chat from a chat", serpis.chat.info().cwd, repo .. "/sub/deep")
    d.key("t", {"cmd"})
    return 1500
  end,
  function()
    T.check("new terminal from a chat", kind(), "terminal")
    T.check("in its folder", here(), repo .. "/sub/deep")
    serpis.command.run("new-chat", repo .. "/sub")
    return 800
  end,
  function()
    -- The folder pill: real click, with a model and thinking kept across the change.
    serpis.chat.set_model(nil, "fake/m2")
    serpis.chat.set_thinking(nil, "low")
    return 500
  end,
  function()
    local r = d.rect("header-folder-button")
    T.check("the header shows the folder", r ~= nil, true)
    click(r)
    return 500
  end,
  function()
    T.check("clicking the folder pill asks for a folder", d.minibuffer(), "Run this chat in: ")
    d.key("Escape")
    T.check("the folder before", serpis.chat.info().cwd, repo .. "/sub")
    T.press(keys("hello") .. " RET")
    return 500
  end,
  function()
    serpis.command.run("chat-select-folder", other)
    T.check("a working chat stays", serpis.chat.info().cwd, repo .. "/sub")
    serpis.chat.abort()
    return 1000
  end,
  function()
    T.check("stopped", serpis.chat.info().running, false)
    serpis.command.run("chat-select-folder", other)
    return 1500
  end,
  function()
    local i = serpis.chat.info()
    T.check("a chat with messages changes folder", i.cwd, other)
    T.check("its messages come along", serpis.buffer.text():find("hello", 1, true) ~= nil, true)
    T.check("its model too", i.model, "fake/m2")
    T.check("saved under the new folder", i.session and i.session:find("other", 1, true) ~= nil, true)
    local s = sh("grep -rl '" .. i.session_id .. "' '" .. agent_dir .. "/sessions' | wc -l")
    T.check("and only there", s:gsub("%s", ""), "1")
    serpis.command.run("new-chat", repo .. "/sub")
    return 800
  end,
  function()
    serpis.chat.set_model(nil, "fake/m2")
    serpis.chat.set_thinking(nil, "low")
    serpis.buffer.set_title(serpis.buffer.current(), "Wholly unrelated")
    d.text("a draft")
    return 500
  end,
  function()
    serpis.command.run("chat-select-folder", other)
    return 800
  end,
  function()
    local i = serpis.chat.info()
    T.check("an empty chat changes folder", i.cwd, other)
    T.check("keeps its model", i.model, "fake/m2")
    T.check("keeps its thinking", i.thinking, "low")
    T.check("keeps what was being written", d.composer_state().text, "a draft")
    serpis.command.run("new-chat", repo)
    serpis.buffer.set_title(serpis.buffer.current(), "Wholly different")
    return 500
  end,
  function()
    -- The switcher finds them by project, shown beside the name.
    d.key("p", {"cmd"})
    return 300
  end,
  function()
    T.press(keys("repo"))
    return 300
  end,
  function()
    local l = labels()
    T.check("typing the project finds its chat", l["Wholly different"] ~= nil, true)
    T.check("and not the one elsewhere", l["Wholly unrelated"], nil)
    T.check("the project stays beside the name", (l["Wholly different"] or ""):find("repo", 1, true) ~= nil, true)
    T.press("ESC")
    sh("rm -rf '" .. tmp .. "'")
    T.finish()
    return 100000
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return end
  local ok, wait = pcall(steps[i])
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
