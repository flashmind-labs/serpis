-- Images in terminals (Kitty graphics protocol): a program prints a PNG with an escape sequence,
-- and serpis draws it over the cells it asked for.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
os.execute("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "'")
-- A 4x4 red PNG, and a script that shows it 10 columns by 5 rows wide (a=T: transmit and show).
os.execute("python3 -c \"import zlib,struct,base64\n"
  .. "def chunk(t,d): return struct.pack('>I',len(d))+t+d+struct.pack('>I',zlib.crc32(t+d)&0xffffffff)\n"
  .. "raw=b''.join(b'\\\\x00'+b'\\\\xff\\\\x00\\\\x00'*4 for _ in range(4))\n"
  .. "png=b'\\\\x89PNG\\\\r\\\\n\\\\x1a\\\\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',4,4,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(raw))+chunk(b'IEND',b'')\n"
  .. "ph=lambda r: chr(0x10EEEE)+chr([0x0305,0x030D][r])+chr(0x0305)+chr(0x10EEEE)*3\n"
  .. "open('" .. tmp .. "/virtual.txt','w').write('\\\\033_Ga=T,U=1,i=42,c=4,r=2,f=100,q=2;'+base64.b64encode(png).decode()+'\\\\033\\\\\\\\'+'\\\\033[38;5;42m'+ph(0)+'\\\\n'+ph(1)+'\\\\033[0m\\\\n')\n"
  .. "open('" .. tmp .. "/show.sh','w').write(\\\"printf '\\\\\\\\033_Ga=T,f=100,c=10,r=5,q=2;\\\" + base64.b64encode(png).decode() + \\\"\\\\\\\\033\\\\\\\\\\\\\\\\'\\\\n\\\")\"")
serpis.command.run("new-terminal")
local buf = serpis.buffer.current()

local function when(ready, f, tries)
  tries = tries or 150
  if ready() or tries == 0 then return f() end
  serpis.defer(100, function() when(ready, f, tries - 1) end)
end

when(function() return serpis.buffer.text():find("%S") ~= nil end, function() serpis.defer(700, function()
  serpis.buffer.insert("clear; sh '" .. tmp .. "/show.sh'\r")
  when(function() return #(d.terminal_images(buf) or { images = {} }).images > 0 end, function()
    local info = d.terminal_images(buf)
    local img = info.images[1]
    T.check("the image is drawn", img ~= nil, true)
    if img then
      T.check("10 columns wide", math.floor(img.w / info.cell.w + 0.5), 10)
      T.check("5 rows tall", math.floor(img.h / info.cell.h + 0.5), 5)
    end
    if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/terminal-image.png") end
    -- Deleting all placements (kitty a=d) removes it.
    serpis.defer(600, function()
      serpis.buffer.insert("printf '\\033_Ga=d\\033\\\\'; clear\r")
      when(function() return #d.terminal_images(buf).images == 0 end, function()
        T.check("deleted images go away", #d.terminal_images(buf).images, 0)
        -- Unicode placeholders (how yazi shows previews): a virtual image, and cells of the
        -- placeholder character colored with its id, 2 rows of 4.
        serpis.buffer.insert("clear; printf '%b' \"$(cat '" .. tmp .. "/virtual.txt')\"\r")
        when(function() return #d.terminal_images(buf).images > 0 end, function()
          local info = d.terminal_images(buf)
          T.check("placeholder rows are drawn", #info.images, 2)
          local w = info.images[1] and math.floor(info.images[1].w / info.cell.w + 0.5)
          T.check("each 4 cells wide", w, 4)
          T.finish()
        end, 50)
      end, 50)
    end)
  end, 80)
end) end)
