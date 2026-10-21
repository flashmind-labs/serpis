-- Images in chats and terminals: drop or paste an image into a chat's composer, remove it with
-- the mouse, send it to a model that takes images (the local mock counts them), see it in the
-- transcript and after resuming; images a tool returns; and pasting an image into a terminal
-- program (it gets the saved file's path, as Claude Code expects).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "'")
-- A 240x160 gradient PNG and a text file.
sh("python3 -c \"import zlib,struct\n"
  .. "def c(t,d): return struct.pack('>I',len(d))+t+d+struct.pack('>I',zlib.crc32(t+d)&0xffffffff)\n"
  .. "W,H=240,160\n"
  .. "raw=b''.join(b'\\\\x00'+b''.join(bytes([x*255//W,y*255//H,180]) for x in range(W)) for y in range(H))\n"
  .. "open('" .. tmp .. "/pic.png','wb').write(b'\\\\x89PNG\\\\r\\\\n\\\\x1a\\\\n'+c(b'IHDR',struct.pack('>IIBBBBB',W,H,8,2,0,0,0))+c(b'IDAT',zlib.compress(raw))+c(b'IEND',b''))\"")
sh("echo hi > '" .. tmp .. "/notes.txt'")
local png = tmp .. "/pic.png"

local agent_dir = os.getenv("SERPIS_AGENT_DIR")
local log = os.getenv("SERPIS_TEST_MOCK_LOG")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":['
  .. '{"id":"vision","input":["text","image"]},{"id":"text-only"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/vision")
serpis.var.set("session-directories", {})

local function lines()
  local out = {}
  local fh = io.open(log)
  if fh then
    for l in fh:lines() do table.insert(out, l) end
    fh:close()
  end
  return out
end
local base = #lines()
local function requests()
  local all, out = lines(), {}
  for i = base + 1, #all do table.insert(out, all[i]) end
  return out
end
local function last_message(prefix)
  local ms = d.messages()
  for i = #ms, 1, -1 do
    if not prefix or ms[i]:sub(1, #prefix) == prefix then return ms[i] end
  end
end
-- Click a composer control as a mouse does: the pointer arrives first, the click a frame later.
local function click(name)
  local r = d.composer()[name]
  if not r then return false end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
  return true
end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local s = list[i]
    if not s then return T.finish() end
    local wait = s(go)
    if wait ~= false then serpis.defer(wait or 300, go) end
  end
  go()
end
local function settled(buf, f, n, tries)
  tries = tries or 0
  local busy = serpis.chat.running(buf) or (n and #requests() < n)
  if busy and tries < 100 then
    return serpis.defer(100, function() settled(buf, f, n, tries + 1) end)
  end
  serpis.defer(300, f)
end

local chat, term
steps({
  function()
    serpis.command.run("new-chat", tmp)
    chat = serpis.buffer.current()
    -- Dropping an image and a text file: the image attaches, the text file's path goes in the prompt.
    d.drop_files({ png, tmp .. "/notes.txt" })
  end,
  function()
    local a = serpis.chat.attachments(chat)
    T.check("dropped image attaches", #a == 1 and a[1].name, "pic.png")
    T.check("with its size", a[1] and (a[1].width .. "x" .. a[1].height), "240x160")
    T.check("other files go in the prompt as paths", d.composer_state().text, tmp .. "/notes.txt")
    T.check("its thumbnail is shown", d.composer()["attachment-0"] ~= nil, true)
  end,
  function(go)
    -- The composer may still be settling after the drop: click again until it's gone.
    local tries = 0
    local function try()
      tries = tries + 1
      click("attachment-0-remove")
      serpis.defer(400, function()
        if #serpis.chat.attachments(chat) > 0 and tries < 3 then return try() end
        go()
      end)
    end
    try()
    return false
  end,
  function()
    T.check("× removes it", #serpis.chat.attachments(chat), 0)
    -- A pasted image (a screenshot on the clipboard) attaches too.
    d.paste_image(png)
  end,
  function()
    local a = serpis.chat.attachments(chat)
    T.check("pasted image attaches", #a == 1 and a[1].name:match("^pasted%-%d+%.png$") ~= nil, true)
    -- A model without image input refuses, keeping the attachment.
    serpis.chat.set_model(chat, "mock/text-only")
  end,
  function()
    d.key("Enter")
  end,
  function()
    T.check("text-only model refuses images", (last_message("error") or ""):find("doesn't take images", 1, true) ~= nil, true)
    T.check("attachment kept", #serpis.chat.attachments(chat), 1)
    T.check("nothing sent", #requests(), 0)
    serpis.chat.set_model(chat, "mock/vision")
  end,
  function(go)
    d.key("Enter")
    settled(chat, go, 1)
    return false
  end,
  function()
    local r = requests()
    T.check("the image reaches the model", r[1] and r[1]:find('"images": 1', 1, true) ~= nil, true)
    T.check("composer emptied", #serpis.chat.attachments(chat), 0)
    T.check("transcript shows the image", serpis.chat.transcript(chat):find("[image 240×160]", 1, true) ~= nil, true)
    if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/chat-image.png") end
    -- An image a tool returns (read on an image file) is shown in its card.
    serpis.chat.prompt(chat, "READ:" .. png)
  end,
  function(go)
    settled(chat, go, 3)
    return false
  end,
  function()
    local t = serpis.chat.transcript(chat)
    local n = select(2, t:gsub("%[image 240×160%]", ""))
    T.check("read's image shows in its card", n, 2)
    if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/chat-read-image.png") end
    -- The session keeps images: resume it and they are back.
    local path = serpis.session.of(chat)
    serpis.buffer.kill(chat)
    chat = serpis.session.resume(path)
  end,
  function()
    local t = serpis.chat.transcript(chat)
    T.check("images come back on resume", select(2, t:gsub("%[image 240×160%]", "")), 2)
    -- A terminal program gets a pasted image as the path of the saved PNG.
    serpis.command.run("new-terminal", tmp)
    term = serpis.buffer.current()
  end,
  function()
    serpis.defer(1200, function()
      serpis.buffer.insert("echo ")
      serpis.defer(300, function()
        d.paste_image(png)
        serpis.defer(300, function() serpis.buffer.insert("\r") end)
      end)
    end)
    return 2600
  end,
  function()
    local out = serpis.buffer.text()
    local path = out:match("\n([^\n]*serpis%-pastes/pasted%-%d+%.png)")
    T.check("terminal gets the saved image's path", path ~= nil, true)
    T.check("and the file is the image", path and sh("file -b '" .. path .. "'"):match("^PNG image data, 240 x 160") ~= nil, true)
  end,
})
