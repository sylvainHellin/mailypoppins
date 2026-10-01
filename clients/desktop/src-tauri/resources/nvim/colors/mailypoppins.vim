" mailypoppins: the desktop app's palette in Neovim and Vim (ticket 0137).
"
" The embedded editor runs `nvim` or `vim` with this directory on the
" runtime path and `-c "set background=<dark|light>" -c "colorscheme
" mailypoppins"`, so the editor shows the app's colours over the pane
" (clients/desktop/docs/shell.md, "The embedded editor").
"
" 'background' picks the palette. Every group sets a gui hex value, read
" with 'termguicolors', and a cterm value, one of the pane's 16 ANSI slots
" or NONE, read without it; Normal's cterm colours are NONE, the pane's own
" foreground and background, which are the same tokens.

highlight clear
if exists('syntax_on')
  syntax reset
endif
let g:colors_name = 'mailypoppins'

let s:cpo_save = &cpo
set cpo&vim

" The design tokens of clients/desktop/src/index.css: `:root` (dark) and
" `:root.light`. src/design/colorscheme.test.ts keeps every value equal to
" index.css and allows no hex value outside these two blocks.
let s:dark = {
      \ 'background': '#0F213D',
      \ 'foreground': '#F4F1E8',
      \ 'muted': '#1A3457',
      \ 'accent': '#1F3D66',
      \ 'primary': '#FF6700',
      \ 'border': '#25406A',
      \ 'destructive': '#FF7A85',
      \ 'selection': '#174A68',
      \ 'terminal-black': '#25406A',
      \ 'terminal-red': '#FF7A85',
      \ 'terminal-green': '#8FD19E',
      \ 'terminal-yellow': '#F2C14E',
      \ 'terminal-blue': '#4BA3CC',
      \ 'terminal-magenta': '#D7A2E8',
      \ 'terminal-cyan': '#5CC8C8',
      \ 'terminal-white': '#B4BFCE',
      \ 'terminal-bright-black': '#7D8BA3',
      \ 'terminal-bright-red': '#FF9AA2',
      \ 'terminal-bright-green': '#B0E3BB',
      \ 'terminal-bright-yellow': '#FF6700',
      \ 'terminal-bright-blue': '#6DB6DA',
      \ 'terminal-bright-magenta': '#E6C3F0',
      \ 'terminal-bright-cyan': '#8EDCDC',
      \ 'terminal-bright-white': '#F4F1E8',
      \ }

let s:light = {
      \ 'background': '#FBFAF6',
      \ 'foreground': '#0C1B33',
      \ 'muted': '#ECE8DD',
      \ 'accent': '#E4DFD0',
      \ 'primary': '#FF6700',
      \ 'border': '#D3D6DC',
      \ 'destructive': '#B4232E',
      \ 'selection': '#CFE3EE',
      \ 'terminal-black': '#0C1B33',
      \ 'terminal-red': '#B4232E',
      \ 'terminal-green': '#2F7A3E',
      \ 'terminal-yellow': '#8A6100',
      \ 'terminal-blue': '#1A6385',
      \ 'terminal-magenta': '#8A3FA0',
      \ 'terminal-cyan': '#1E7A7A',
      \ 'terminal-white': '#E6E1D3',
      \ 'terminal-bright-black': '#77839A',
      \ 'terminal-bright-red': '#D6404B',
      \ 'terminal-bright-green': '#3E9450',
      \ 'terminal-bright-yellow': '#B34700',
      \ 'terminal-bright-blue': '#1F6F93',
      \ 'terminal-bright-magenta': '#A35BB8',
      \ 'terminal-bright-cyan': '#2A9494',
      \ 'terminal-bright-white': '#FFFFFF',
      \ }

let s:light_palette = &background ==# 'light'
let s:t = s:light_palette ? s:light : s:dark

" The roles the groups take, each [gui hex, cterm]. A terminal slot keeps
" its own ANSI index; a surface token takes the slot nearest it in its
" palette, which is why some cterm values differ between the two.
let s:r = {
      \ 'fg': [s:t['foreground'], 'NONE'],
      \ 'bg': [s:t['background'], 'NONE'],
      \ 'surface': [s:t['muted'], s:light_palette ? 7 : 0],
      \ 'raised': [s:t['accent'], s:light_palette ? 7 : 0],
      \ 'line': [s:t['border'], s:light_palette ? 7 : 0],
      \ 'visual': [s:t['selection'], s:light_palette ? 7 : 0],
      \ 'cursorline': [s:t['muted'], 'NONE'],
      \ 'pick': [s:t['selection'], 4],
      \ 'pick_fg': [s:t['foreground'], s:light_palette ? 15 : 0],
      \ 'ink': [s:t['background'], s:light_palette ? 15 : 0],
      \ 'primary': [s:t['primary'], 11],
      \ 'on_primary': [s:light_palette ? s:t['foreground'] : s:t['background'], s:light_palette ? 15 : 0],
      \ 'error': [s:t['destructive'], 1],
      \ 'dim': [s:t['terminal-bright-black'], 8],
      \ 'red': [s:t['terminal-red'], 1],
      \ 'green': [s:t['terminal-green'], 2],
      \ 'yellow': [s:t['terminal-yellow'], 3],
      \ 'blue': [s:t['terminal-blue'], 4],
      \ 'magenta': [s:t['terminal-magenta'], 5],
      \ 'cyan': [s:t['terminal-cyan'], 6],
      \ 'orange': [s:t['terminal-bright-yellow'], 11],
      \ 'link': [s:t['terminal-bright-blue'], 12],
      \ }

" s:hi(group, fg, bg [, attr [, sp]]): roles by name, '' for NONE; attr
" such as 'bold,italic' goes to gui and cterm, undercurl as underline in
" cterm.
function! s:hi(group, fg, bg, ...) abort
  let l:fg = a:fg ==# '' ? ['NONE', 'NONE'] : s:r[a:fg]
  let l:bg = a:bg ==# '' ? ['NONE', 'NONE'] : s:r[a:bg]
  let l:attr = a:0 > 0 && a:1 !=# '' ? a:1 : 'NONE'
  let l:cmd = 'highlight ' . a:group
        \ . ' guifg=' . l:fg[0] . ' guibg=' . l:bg[0]
        \ . ' ctermfg=' . l:fg[1] . ' ctermbg=' . l:bg[1]
        \ . ' gui=' . l:attr . ' cterm=' . substitute(l:attr, 'undercurl', 'underline', 'g')
  if a:0 > 1 && a:2 !=# ''
    let l:cmd .= ' guisp=' . s:r[a:2][0]
  endif
  execute l:cmd
endfunction

" The editor.
call s:hi('Normal', 'fg', 'bg')
call s:hi('NormalNC', 'fg', 'bg')
call s:hi('NormalFloat', 'fg', 'surface')
call s:hi('FloatBorder', 'dim', 'surface')
call s:hi('FloatTitle', 'orange', 'surface', 'bold')
call s:hi('Cursor', 'bg', 'fg')
call s:hi('TermCursor', '', '', 'reverse')
call s:hi('LineNr', 'dim', '')
call s:hi('CursorLine', '', 'cursorline')
call s:hi('CursorColumn', '', 'cursorline')
call s:hi('CursorLineNr', 'orange', '', 'bold')
call s:hi('ColorColumn', '', 'raised')
call s:hi('SignColumn', 'dim', '')
call s:hi('FoldColumn', 'dim', '')
call s:hi('Folded', 'dim', 'surface')
call s:hi('NonText', 'line', '')
call s:hi('EndOfBuffer', 'line', '')
call s:hi('Whitespace', 'line', '')
call s:hi('SpecialKey', 'line', '')
call s:hi('Conceal', 'dim', '')
call s:hi('Visual', '', 'visual')
call s:hi('VisualNOS', '', 'visual')
call s:hi('Search', 'ink', 'yellow')
call s:hi('IncSearch', 'on_primary', 'primary', 'bold')
call s:hi('CurSearch', 'on_primary', 'primary', 'bold')
call s:hi('Substitute', 'on_primary', 'primary')
call s:hi('MatchParen', 'orange', '', 'bold')
call s:hi('QuickFixLine', '', 'raised')
call s:hi('Directory', 'blue', '')
call s:hi('Title', 'orange', '', 'bold')

" Menus, bars and splits.
call s:hi('Pmenu', 'fg', 'surface')
call s:hi('PmenuSel', 'pick_fg', 'pick', 'bold')
call s:hi('PmenuSbar', '', 'surface')
call s:hi('PmenuThumb', '', 'dim')
call s:hi('PmenuKind', 'blue', 'surface')
call s:hi('PmenuExtra', 'dim', 'surface')
call s:hi('WildMenu', 'pick_fg', 'pick', 'bold')
call s:hi('StatusLine', 'fg', 'surface', 'bold')
call s:hi('StatusLineNC', 'dim', 'surface')
call s:hi('StatusLineTerm', 'fg', 'surface', 'bold')
call s:hi('StatusLineTermNC', 'dim', 'surface')
call s:hi('TabLine', 'dim', 'surface')
call s:hi('TabLineFill', '', 'surface')
call s:hi('TabLineSel', 'fg', 'bg', 'bold')
call s:hi('WinBar', 'fg', '', 'bold')
call s:hi('WinBarNC', 'dim', '')
call s:hi('VertSplit', 'line', '')
call s:hi('WinSeparator', 'line', '')

" Messages.
call s:hi('ErrorMsg', 'error', '', 'bold')
call s:hi('WarningMsg', 'yellow', '')
call s:hi('MoreMsg', 'green', '')
call s:hi('OkMsg', 'green', '')
call s:hi('Question', 'green', '')
call s:hi('ModeMsg', 'fg', '', 'bold')
call s:hi('MsgArea', 'fg', '')

" Spelling.
call s:hi('SpellBad', '', '', 'undercurl', 'red')
call s:hi('SpellCap', '', '', 'undercurl', 'blue')
call s:hi('SpellRare', '', '', 'undercurl', 'magenta')
call s:hi('SpellLocal', '', '', 'undercurl', 'cyan')

" Diffs.
call s:hi('DiffAdd', 'green', '')
call s:hi('DiffDelete', 'red', '')
call s:hi('DiffChange', '', 'surface')
call s:hi('DiffText', 'yellow', 'surface', 'bold')
call s:hi('Added', 'green', '')
call s:hi('Changed', 'yellow', '')
call s:hi('Removed', 'red', '')
call s:hi('diffAdded', 'green', '')
call s:hi('diffRemoved', 'red', '')
call s:hi('diffChanged', 'yellow', '')
call s:hi('diffLine', 'cyan', '')
call s:hi('diffFile', 'fg', '', 'bold')

" Syntax, restrained: a few hues, most text in the foreground.
call s:hi('Comment', 'dim', '', 'italic')
call s:hi('Constant', 'magenta', '')
call s:hi('String', 'green', '')
call s:hi('Character', 'green', '')
call s:hi('Number', 'magenta', '')
call s:hi('Boolean', 'magenta', '')
call s:hi('Float', 'magenta', '')
call s:hi('Identifier', 'blue', '')
call s:hi('Function', 'blue', '')
call s:hi('Statement', 'orange', '')
call s:hi('Operator', 'fg', '')
call s:hi('PreProc', 'magenta', '')
call s:hi('Type', 'cyan', '')
call s:hi('Special', 'cyan', '')
call s:hi('Delimiter', 'dim', '')
call s:hi('SpecialComment', 'dim', '', 'italic')
call s:hi('Underlined', 'link', '', 'underline')
call s:hi('Ignore', 'line', '')
call s:hi('Error', 'error', '', 'bold')
call s:hi('Todo', 'orange', '', 'bold')

" Markdown, Vim's syntax.
call s:hi('markdownH1', 'orange', '', 'bold')
call s:hi('markdownH2', 'orange', '', 'bold')
call s:hi('markdownH3', 'orange', '', 'bold')
call s:hi('markdownH4', 'orange', '', 'bold')
call s:hi('markdownH5', 'orange', '', 'bold')
call s:hi('markdownH6', 'orange', '', 'bold')
call s:hi('markdownHeadingDelimiter', 'orange', '')
call s:hi('markdownHeadingRule', 'dim', '')
call s:hi('markdownRule', 'dim', '')
call s:hi('markdownBold', 'fg', '', 'bold')
call s:hi('markdownItalic', 'fg', '', 'italic')
call s:hi('markdownBoldItalic', 'fg', '', 'bold,italic')
call s:hi('markdownStrike', 'dim', '', 'strikethrough')
call s:hi('markdownBlockquote', 'dim', '', 'italic')
call s:hi('markdownListMarker', 'orange', '')
call s:hi('markdownOrderedListMarker', 'orange', '')
call s:hi('markdownLinkText', 'blue', '')
call s:hi('markdownUrl', 'link', '', 'underline')
call s:hi('markdownAutomaticLink', 'link', '', 'underline')
call s:hi('markdownLinkTextDelimiter', 'dim', '')
call s:hi('markdownLinkDelimiter', 'dim', '')
call s:hi('markdownUrlDelimiter', 'dim', '')
call s:hi('markdownCode', 'cyan', '')
call s:hi('markdownCodeBlock', 'cyan', '')
call s:hi('markdownCodeDelimiter', 'dim', '')
call s:hi('markdownFootnote', 'dim', '')
call s:hi('markdownEscape', 'cyan', '')
call s:hi('markdownError', 'fg', '')

" The draft's YAML frontmatter, Vim's syntax.
call s:hi('yamlBlockMappingKey', 'blue', '')
call s:hi('yamlFlowMappingKey', 'blue', '')
call s:hi('yamlKeyValueDelimiter', 'dim', '')
call s:hi('yamlDocumentStart', 'dim', '')
call s:hi('yamlDocumentEnd', 'dim', '')
call s:hi('yamlPlainScalar', 'fg', '')

" Neovim only: tree-sitter captures and diagnostics. Vim refuses `@` in a
" group name.
if has('nvim')
  call s:hi('@variable', 'fg', '')
  call s:hi('@property', 'blue', '')
  call s:hi('@punctuation.delimiter', 'dim', '')
  call s:hi('@punctuation.bracket', 'dim', '')
  call s:hi('@punctuation.special', 'orange', '')
  call s:hi('@markup.heading', 'orange', '', 'bold')
  call s:hi('@markup.strong', 'fg', '', 'bold')
  call s:hi('@markup.italic', 'fg', '', 'italic')
  call s:hi('@markup.strikethrough', 'dim', '', 'strikethrough')
  call s:hi('@markup.underline', 'fg', '', 'underline')
  call s:hi('@markup.quote', 'dim', '', 'italic')
  call s:hi('@markup.math', 'cyan', '')
  call s:hi('@markup.link', 'dim', '')
  call s:hi('@markup.link.label', 'blue', '')
  call s:hi('@markup.link.url', 'link', '', 'underline')
  call s:hi('@markup.raw', 'cyan', '')
  call s:hi('@markup.raw.block', 'cyan', '')
  call s:hi('@markup.list', 'orange', '')
  call s:hi('@markup.list.checked', 'green', '')
  call s:hi('@markup.list.unchecked', 'dim', '')
  call s:hi('DiagnosticError', 'red', '')
  call s:hi('DiagnosticWarn', 'yellow', '')
  call s:hi('DiagnosticInfo', 'blue', '')
  call s:hi('DiagnosticHint', 'cyan', '')
  call s:hi('DiagnosticOk', 'green', '')
  call s:hi('DiagnosticUnderlineError', '', '', 'undercurl', 'red')
  call s:hi('DiagnosticUnderlineWarn', '', '', 'undercurl', 'yellow')
  call s:hi('DiagnosticUnderlineInfo', '', '', 'undercurl', 'blue')
  call s:hi('DiagnosticUnderlineHint', '', '', 'undercurl', 'cyan')
  call s:hi('DiagnosticUnderlineOk', '', '', 'undercurl', 'green')
endif

let &cpo = s:cpo_save
unlet s:cpo_save s:light_palette s:t s:r
