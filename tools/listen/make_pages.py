# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Build the two single-file listening pages (audio embedded as base64 webm-opus). Keys are never read.
  uv run tools/listen/make_pages.py DIR_WITH_P1_WEBM DIR_WITH_P2_WEBM OUT_DIR
"""
import base64, os, sys, json

CSS = """
:root{--bg:#f3f4f2;--fg:#1d2321;--mut:#5b6762;--card:#fff;--line:#cfd6d2;--acc:#1d6b57;--accfg:#fff;--warn:#8a4b00}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){--bg:#121715;--fg:#e4eae7;--mut:#98a59f;--card:#1a211f;--line:#34403b;--acc:#5fc4a6;--accfg:#0c1411;--warn:#e0a35a;color-scheme:dark}}
:root[data-theme="dark"]{--bg:#121715;--fg:#e4eae7;--mut:#98a59f;--card:#1a211f;--line:#34403b;--acc:#5fc4a6;--accfg:#0c1411;--warn:#e0a35a;color-scheme:dark}
body{background:var(--bg);color:var(--fg);font:16px/1.5 Inter,system-ui,sans-serif;padding-block:24px;padding-left:max(16px,env(safe-area-inset-left,0px));padding-right:max(16px,env(safe-area-inset-right,0px))}
main{max-width:46rem;margin:0 auto;display:flex;flex-direction:column;gap:20px}
h1{font:600 1.7rem/1.2 "Space Grotesk",Inter,sans-serif;margin:0;text-wrap:balance}
h2{font:600 1.1rem "Space Grotesk",Inter,sans-serif;margin:0}
p{margin:0;max-width:65ch} .mut{color:var(--mut);font-size:.9rem}
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:16px;display:flex;flex-direction:column;gap:12px;min-width:0}
.pl{display:grid;grid-template-columns:repeat(auto-fit,minmax(15rem,1fr));gap:12px}
.pl div{display:flex;flex-direction:column;gap:4px;min-width:0} audio{width:100%}
.lab{font:600 .8rem/1 "Space Grotesk",sans-serif;letter-spacing:.08em;text-transform:uppercase;color:var(--mut)}
fieldset{border:0;padding:0;margin:0;display:flex;flex-wrap:wrap;gap:8px 16px} legend{padding:0;margin-bottom:6px;font-weight:500}
label{display:inline-flex;gap:6px;align-items:center}
textarea{width:100%;box-sizing:border-box;min-height:3.2rem;background:var(--bg);color:var(--fg);border:1px solid var(--line);border-radius:6px;padding:8px;font:inherit}
button{background:var(--acc);color:var(--accfg);border:0;border-radius:6px;padding:10px 16px;font:600 1rem Inter,sans-serif;cursor:pointer}
button:focus-visible,input:focus-visible,textarea:focus-visible,audio:focus-visible{outline:2px solid var(--acc);outline-offset:2px}
.bar{display:flex;gap:12px;align-items:center;flex-wrap:wrap} pre{overflow-x:auto;background:var(--card);border:1px solid var(--line);padding:12px;border-radius:6px;margin:0;font-size:.8rem}
.warn{color:var(--warn)}
"""
HEAD = '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&family=Space+Grotesk:wght@600&display=swap">'

JS = """
(function(){
var KEY='%KEY%', ITEMS=%ITEMS%, state={}, dbc=null;
try{state=JSON.parse(localStorage.getItem(KEY)||'{}')}catch(e){}
function save(id){try{localStorage.setItem(KEY,JSON.stringify(state))}catch(e){}
  if(dbc){dbc.doc(id).set(Object.assign({savedAt:Date.now()},state[id]||{})).catch(function(){})}
  document.getElementById('st').textContent='Saved '+new Date().toLocaleTimeString()}
document.querySelectorAll('[data-item]').forEach(function(card){
  var id=card.getAttribute('data-item'); var s=state[id]||{};
  card.querySelectorAll('input[type=radio]').forEach(function(r){r.checked=(s[r.name.split(':')[1]]===r.value);
    r.addEventListener('change',function(){state[id]=state[id]||{};state[id][r.name.split(':')[1]]=r.value;save(id)})});
  var t=card.querySelector('textarea'); if(t){t.value=s.note||'';t.addEventListener('input',function(){state[id]=state[id]||{};state[id].note=t.value;save(id)})}
});
function out(){var o={};ITEMS.forEach(function(i){o[i]=state[i]||{}});return JSON.stringify({page:KEY,answers:o},null,1)}
document.getElementById('copy').addEventListener('click',function(){var txt=out(),box=document.getElementById('box');box.hidden=false;box.textContent=txt;
  var ok=function(){document.getElementById('st').textContent='Copied'};
  try{navigator.clipboard.writeText(txt).then(ok,function(){var r=document.createRange();r.selectNodeContents(box);var s=getSelection();s.removeAllRanges();s.addRange(r)})}catch(e){}});
if(window.claude&&claude.use){claude.use('db').then(function(db){if(db){dbc=db.collection('answers_'+KEY);document.getElementById('st').textContent='Answers also sync to the page owner.'}}).catch(function(){})}
})();
"""

def b64(p): return base64.b64encode(open(p,'rb').read()).decode()
def player(lbl,p): return f'<div><span class="lab">{lbl}</span><audio controls preload="auto" src="data:audio/webm;codecs=opus;base64,{b64(p)}"></audio></div>'
def radios(id,name,q,opts): return f'<fieldset><legend>{q}</legend>'+''.join(f'<label><input type="radio" name="{id}:{name}" value="{v}"> {t}</label>' for v,t in opts)+'</fieldset>'

def page(title,key,intro,cards,ids):
    js=JS.replace('%KEY%',key).replace('%ITEMS%',json.dumps(ids))
    return f"""<title>{title}</title>
<meta name="claude:viewport-fit-sides" content="cover">
{HEAD}<style>{CSS}</style>
<main><h1>{title}</h1>{intro}{cards}
<div class="card"><div class="bar"><button id="copy" type="button">Copy my answers as JSON</button><span id="st" class="mut">Answers save in this browser as you go.</span></div><pre id="box" hidden></pre></div></main>
<script>{js}</script>"""

def p1(d,out):
    cards=[];ids=[]
    conds=["noloss","loss2","loss5"]
    for n in range(1,7):
        pid=f"P{n}";ids.append(pid)
        cards.append(f'''<section class="card" data-item="{pid}"><h2>Pair {n} of 6</h2>
<div class="pl">{player("A",f"{d}/{pid}-A.webm")}{player("B",f"{d}/{pid}-B.webm")}</div>
{radios(pid,"natural","Which sounds more natural?",[("A","A"),("B","B"),("same","Same")])}
{radios(pid,"glitch","Any click, gap or missing syllable?",[("A","In A"),("B","In B"),("both","Both"),("neither","Neither")])}
<textarea id="note-{pid}" aria-label="Note for pair {n}" placeholder="Where in the clip, what it sounded like (optional)"></textarea></section>''')
    intro='''<p>Six pairs of the same 12-second voice clip, played through two versions of the voice receiver. Some pairs had packets dropped on the way, some none. A and B are in random order and the key stays in a separate file.</p>
<p class="mut">Use headphones, at the volume you would talk with. Every file has the same loudness. About 15 minutes. Do not use the speed controls; listen to each at least twice. The "no loss" pairs should sound the same, so answer "Same" if you cannot tell.</p>'''
    open(f"{out}/prim-voice-receiver-listening-1.html","w").write(page("Voice receiver A/B","prim-recv-p1",intro,"".join(cards),ids))

def p2(d,out):
    cards=[];ids=[]
    for c in range(3):
        labs=[f"X{c*3+i+1}" for i in range(3)];cid=f"clip{c+1}";ids.append(cid)
        cards.append(f'''<section class="card" data-item="{cid}"><h2>Clip {c+1}: three versions</h2>
<div class="pl">{"".join(player(l,f"{d}/{l}.webm") for l in labs)}</div>
{radios(cid,"best","Which do you prefer?",[(l,l) for l in labs]+[("none","No difference")])}
{radios(cid,"worst","Which is worst?",[(l,l) for l in labs]+[("none","No difference")])}
<textarea id="note-{cid}" aria-label="Note for clip {c+1}" placeholder="What differs: sibilants, breathiness, metallic ring (optional)"></textarea></section>''')
    intro='''<p>Three short speech clips, each encoded three ways at different bitrates (labels hide which). Pick the one you like most and the one you like least in each trio.</p>
<p class="mut">Headphones, normal talking volume; all files are loudness matched. About 10 minutes. This informs a later bitrate decision and does not block anything.</p>'''
    open(f"{out}/prim-voice-bitrate-listening-2.html","w").write(page("Voice bitrate trios","prim-recv-p2",intro,"".join(cards),ids))

p1(sys.argv[1],sys.argv[3]);p2(sys.argv[2],sys.argv[3])
