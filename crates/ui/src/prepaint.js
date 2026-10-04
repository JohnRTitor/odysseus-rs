  // Pre-paint theme script, extracted verbatim from static/index.html:19-71.
  // Runs before first paint: the saved theme's CSS custom properties must be on the
  // root element before the browser paints, or the user sees a flash of the wrong theme.
  // Do not 'improve' this -- crates/ui/src/theme.rs is the tested Rust twin of the
  // arithmetic below, and a divergence between them is invisible until a user reports
  // wrong colours.
    (function(){
      try {
        var t = JSON.parse(localStorage.getItem('odysseus-theme'));
        if (t && t.colors) {
          var s = document.documentElement.style;
          var c = t.colors;
          s.setProperty('--bg', c.bg);
          s.setProperty('--fg', c.fg);
          s.setProperty('--panel', c.panel);
          s.setProperty('--border', c.border);
          if (c.red) s.setProperty('--red', c.red);
          // Set --brand-color now (= custom brand or the theme red) so the
          // loading-screen ASCII wave shows its final colour from the first
          // paint. Otherwise it starts on --red and visibly switches once the
          // app boots and applyColors() fills --brand-color.
          var _bc = (c.advanced && c.advanced.brandColor) || c.red;
          if (_bc) s.setProperty('--brand-color', _bc);
          // Match the mobile browser toolbar / status bar to the saved theme bg
          // from the very first paint. Otherwise the hardcoded dark meta sits
          // there during load and the bar "switches" colour once the app boots.
          var mtc = document.querySelector('meta[name="theme-color"]');
          if (mtc && c.bg) mtc.setAttribute('content', c.bg);
          // Update favicon to match accent color
          var ac = c.red || '#e06c75';
          var fav = document.querySelector("link[rel='icon']");
          if (fav) fav.href = 'data:image/svg+xml,' + encodeURIComponent("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'><path d='M16 4L16 22L6 22Z' fill='" + ac + "'/><path d='M16 8L16 22L24 22Z' fill='" + ac + "' opacity='0.6'/><path d='M4 24Q10 20 16 24Q22 28 28 24' stroke='" + ac + "' stroke-width='2.5' fill='none' stroke-linecap='round'/></svg>");
          // Derive syntax highlighting colors from theme
          function h2hsl(hex) {
            hex = hex.replace('#','');
            var r=parseInt(hex.substring(0,2),16)/255, g=parseInt(hex.substring(2,4),16)/255, b=parseInt(hex.substring(4,6),16)/255;
            var mx=Math.max(r,g,b), mn=Math.min(r,g,b), h, sv, l=(mx+mn)/2;
            if(mx===mn){h=sv=0}else{var d=mx-mn;sv=l>0.5?d/(2-mx-mn):d/(mx+mn);if(mx===r)h=((g-b)/d+(g<b?6:0))/6;else if(mx===g)h=((b-r)/d+2)/6;else h=((r-g)/d+4)/6}
            return[h*360,sv*100,l*100];
          }
          function hsl2h(h,sv,l) {
            h=((h%360)+360)%360;sv=Math.max(0,Math.min(100,sv))/100;l=Math.max(0,Math.min(100,l))/100;
            var a=sv*Math.min(l,1-l);function f(n){var k=(n+h/30)%12;return l-a*Math.max(-1,Math.min(k-3,9-k,1))}
            function th(v){return Math.round(v*255).toString(16).padStart(2,'0')}
            return'#'+th(f(0))+th(f(8))+th(f(4));
          }
          var fH=h2hsl(c.fg),bH=h2hsl(c.bg),rH=h2hsl(c.red||'#e06c75');
          var dk=bH[2]<50;
          s.setProperty('--hl-bg', hsl2h(bH[0],bH[1],dk?Math.max(bH[2]-4,0):Math.min(bH[2]+4,100)));
          s.setProperty('--hl-fg', c.fg);
          s.setProperty('--hl-keyword', hsl2h((rH[0]+280)%360,Math.min(rH[1]+10,80),dk?70:45));
          s.setProperty('--hl-string', hsl2h(40,Math.min(fH[1]+20,70),dk?72:42));
          s.setProperty('--hl-comment', hsl2h(fH[0],Math.max(fH[1]-20,5),(fH[2]*0.5+bH[2]*0.5)));
          s.setProperty('--hl-function', hsl2h(210,Math.min(fH[1]+20,75),dk?70:45));
          s.setProperty('--hl-number', hsl2h(20,Math.min(fH[1]+15,65),dk?68:48));
          s.setProperty('--hl-builtin', hsl2h(180,Math.min(fH[1]+15,60),dk?65:40));
          s.setProperty('--hl-variable', hsl2h((fH[0]+30)%360,Math.min(fH[1]+5,60),fH[2]));
          s.setProperty('--hl-params', hsl2h(fH[0],Math.max(fH[1]-5,10),dk?Math.min(fH[2]+8,85):Math.max(fH[2]-8,25)));
          // Apply advanced overrides if present
