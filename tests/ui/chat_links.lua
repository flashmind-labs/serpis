local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
sh("mkdir -p '" .. tmp .. "'")
sh("python3 -c \"import zlib,struct\n"
  .. "def c(t,d): return struct.pack('>I',len(d))+t+d+struct.pack('>I',zlib.crc32(t+d)&0xffffffff)\n"
  .. "W,H=240,160\n"
  .. "raw=b''.join(b'\\\\x00'+b''.join(bytes([x*255//W,y*255//H,180]) for x in range(W)) for y in range(H))\n"
  .. "open('" .. tmp .. "/pic.png','wb').write(b'\\\\x89PNG\\\\r\\\\n\\\\x1a\\\\n'+c(b'IHDR',struct.pack('>IIBBBBB',W,H,8,2,0,0,0))+c(b'IDAT',zlib.compress(raw))+c(b'IEND',b''))\"")
local opened = tmp .. "/opened.txt"
local browser = tmp .. "/browser.sh"
local fb = io.open(browser, "w")
fb:write("#!/bin/sh\necho \"$1\" >> '" .. opened .. "'\n")
fb:close()
sh("chmod +x '" .. browser .. "'")

local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"vision","input":["text","image"]}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/vision")
serpis.var.set("session-directories", {})
serpis.var.set("browse-url-program", browser)

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(300, go)
end
local function read(path)
  local fh = io.open(path)
  if not fh then return "" end
  local s = fh:read("*a")
  fh:close()
  return s
end
local function line_with(pred)
  for _, l in ipairs(d.chat_lines()) do
    if pred(l) then return l end
  end
end
local function click_at(x, y)
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end
local function shot(name)
  if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/" .. name .. ".png") end
end
local function linked(l, url)
  for _, u in ipairs(l.links or {}) do
    if u == url then return true end
  end
  return false
end

serpis.command.run("new-chat")
serpis.defer(600, function()
  serpis.chat.attach(nil, tmp .. "/pic.png")
  T.press("L I N K S RET")
  when(function() return not serpis.chat.running() and (serpis.chat.transcript() or ""):find("are here") ~= nil end, function()
    local l = line_with(function(l) return linked(l, "https://example.com/docs") end)
    T.check("a Markdown link keeps where it goes", l ~= nil, true)
    T.check("a bare URL is a link too", l ~= nil and linked(l, "https://example.org/page"), true)
    if not (l and l.origin) then return T.finish() end
    d.mouse_move(l.origin.x + 12, l.origin.y + 8)
    when(function() return d.chat_hovered_link() ~= nil end, function()
    T.check("hovering a link says where it goes", d.chat_hovered_link(), "https://example.com/docs")
    shot("link-hover")
    serpis.defer(400, function() click_at(l.origin.x + 12, l.origin.y + 8) end)
    when(function() return read(opened) ~= "" end, function()
      T.check("clicking a link opens it", read(opened), "https://example.com/docs\n")
      local img = line_with(function(x) return x.image ~= nil end)
      T.check("the image is in the transcript", img ~= nil, true)
      if not img then return T.finish() end
      click_at(img.image.x + img.image.w / 2, img.image.y + img.image.h / 2)
      when(function() return serpis.chat.zoomed_image() ~= nil end, function()
        local z = serpis.chat.zoomed_image()
        T.check("clicking an image shows it enlarged", z and (z.w .. "x" .. z.h), "240x160")
        shot("image-popup")
        serpis.defer(500, function() T.press("ESC") end)
        when(function() return serpis.chat.zoomed_image() == nil end, function()
          T.check("Esc closes it", serpis.chat.zoomed_image(), nil)
          click_at(img.image.x + img.image.w / 2, img.image.y + img.image.h / 2)
          when(function() return serpis.chat.zoomed_image() ~= nil end, function()
            T.check("it opens again", serpis.chat.zoomed_image() ~= nil, true)
            click_at(img.image.x + img.image.w / 2, img.image.y + img.image.h / 2)
            when(function() return serpis.chat.zoomed_image() == nil end, function()
              T.check("a click closes it", serpis.chat.zoomed_image(), nil)
              T.finish()
            end)
          end)
        end)
      end)
    end)
    end)
  end)
end)
