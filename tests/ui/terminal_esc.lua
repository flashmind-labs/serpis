local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
serpis.keys.scheme("vim")
serpis.var.set("confirm-quit-processes", "never")
serpis.command.run("new-terminal")
local term = serpis.buffer.current()

local function when(ready, f, n)
  n = n or 150
  if ready() or n == 0 then return f() end
  serpis.defer(100, function() when(ready, f, n - 1) end)
end
local function running() local p = serpis.buffer.process(term); return p and p.running end
local function state() return serpis.buffer.modes()[1] end
local function last_message() local m = serpis.debug.messages(); return m[#m] end

when(function() return serpis.buffer.text():find("%S") end, function() serpis.defer(700, function()
  serpis.buffer.insert("sleep 30\r")
  when(running, function()
    -- C-c is a prefix in terminals; C-c C-c sends the real one.
    T.press("C-c")
    serpis.defer(700, function()
      T.check("lone C-c waits", running(), true)
      T.press("C-c")
      when(function() return not running() end, function()
        T.check("C-c C-c interrupts", running(), false)
        -- auto on the main screen: ESC enters normal state.
        T.press("ESC")
        serpis.defer(200, function()
          T.check("auto: ESC -> normal on the shell", state(), "vim-normal")
          T.press("i")
          -- A full-screen program: ESC goes to it.
          serpis.buffer.insert("printf '\\033[?1049h'; sleep 30\r")
          when(running, function() serpis.defer(300, function()
            T.press("ESC")
            serpis.defer(200, function()
              T.check("auto: ESC -> program when full-screen", state(), "vim-insert")
              T.press("C-c ESC")
              serpis.defer(200, function()
                T.check("C-c ESC -> normal state instead", state(), "vim-normal")
                T.press("i")
                -- C-c C-z cycles: auto -> terminal -> serpis -> auto.
                T.press("C-c C-z")
                serpis.defer(200, function()
                  T.check("cycle to terminal", last_message():find("always to the program") ~= nil, true)
                  T.press("C-c C-z")
                  serpis.defer(200, function()
                    T.check("cycle to serpis", last_message():find("always to serpis") ~= nil, true)
                    T.press("ESC")
                    serpis.defer(200, function()
                      T.check("serpis: ESC -> normal even when full-screen", state(), "vim-normal")
                      T.press("i C-c C-c")
                      T.finish()
                    end)
                  end)
                end)
              end)
            end)
          end) end)
        end)
      end)
    end)
  end)
end) end)
