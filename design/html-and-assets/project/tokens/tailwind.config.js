// tailwind.config.js — Yarngo
module.exports = {
  theme: {
    extend: {
      colors: {
        orange: { 50:'#FFF3E6',100:'#FFE2C2',200:'#FFCB93',300:'#FFB264',400:'#FF9C3D',500:'#FF8A1F',600:'#E6740F',700:'#BF5C08',800:'#8F4406',900:'#5E2C03' },
        ink:    { 50:'#FFF9F2',100:'#F7F1E8',200:'#EBE4D9',300:'#D8D0C4',400:'#B0A79B',500:'#857D72',600:'#5F594F',700:'#423E37',800:'#2A2723',900:'#171717' },
        leaf:   { 50:'#E9F5EF',100:'#C9E7D9',300:'#7FC3A5',500:'#287A57',700:'#1B5C41',900:'#0E3A28' },
        signal: { 50:'#EEF0FE',100:'#D8DDFD',300:'#9AA6FB',500:'#596DF8',700:'#3A4CD1',900:'#23308F' },
        warning:'#C97A00', danger:'#C7362B',
      },
      fontFamily: {
        display: ['Sora','system-ui','sans-serif'],
        sans: ['"Noto Sans"','system-ui','sans-serif'],
        mono: ['"Noto Sans Mono"','ui-monospace','monospace'],
      },
      borderRadius: { xs:'4px', sm:'8px', md:'12px', lg:'16px', xl:'24px', '2xl':'32px', full:'999px' },
      boxShadow: {
        1:'0 1px 2px rgba(23,23,23,.06)',
        2:'0 4px 16px rgba(23,23,23,.08)',
        3:'0 12px 32px rgba(23,23,23,.12)',
      },
      transitionTimingFunction: { standard:'cubic-bezier(.2,0,0,1)' },
      transitionDuration: { instant:'120ms', fast:'180ms', base:'240ms', slow:'320ms' },
    },
  },
};
