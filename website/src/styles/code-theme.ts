// Syntax theme for every code block on the site, drawn from the desktop app's
// dark palette (clients/desktop/src/index.css, the embedded terminal's ANSI
// colours). Code blocks stay on Prussian in both site themes, so one theme serves.
export const codeTheme = {
  name: 'mailypoppins',
  type: 'dark',
  colors: {
    'editor.background': '#081426',
    'editor.foreground': '#F4F1E8',
  },
  tokenColors: [
    { scope: ['comment', 'punctuation.definition.comment'], settings: { foreground: '#7D8BA3', fontStyle: 'italic' } },
    { scope: ['string', 'markup.inline.raw', 'markup.raw'], settings: { foreground: '#8FD19E' } },
    { scope: ['constant', 'constant.numeric', 'constant.language', 'support.constant'], settings: { foreground: '#F2C14E' } },
    { scope: ['keyword', 'storage', 'keyword.operator', 'support.function.builtin'], settings: { foreground: '#6DB6DA' } },
    { scope: ['entity.name.function', 'support.function', 'entity.name.command'], settings: { foreground: '#5CC8C8' } },
    { scope: ['entity.name.tag', 'support.type.property-name', 'entity.name.type', 'variable.other.key', 'keyword.other.definition'], settings: { foreground: '#6DB6DA' } },
    { scope: ['entity.name.section', 'markup.heading', 'punctuation.definition.heading'], settings: { foreground: '#FF6700', fontStyle: 'bold' } },
    { scope: ['markup.bold'], settings: { fontStyle: 'bold' } },
    { scope: ['markup.italic'], settings: { fontStyle: 'italic' } },
    { scope: ['markup.underline.link', 'string.other.link'], settings: { foreground: '#6DB6DA' } },
    { scope: ['variable', 'variable.other', 'variable.parameter'], settings: { foreground: '#F4F1E8' } },
    { scope: ['punctuation', 'meta.brace'], settings: { foreground: '#B4BFCE' } },
  ],
};
