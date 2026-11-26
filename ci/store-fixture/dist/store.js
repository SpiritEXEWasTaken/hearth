
const q=document.getElementById('q'),cards=[...document.querySelectorAll('.card')],chips=[...document.querySelectorAll('.chip')];let cat='';
function f(){const t=q.value.trim().toLowerCase();for(const c of cards){const ok=(!cat||c.dataset.cat===cat)&&(!t||c.dataset.text.includes(t));c.style.display=ok?'':'none'}}
q.addEventListener('input',f);for(const ch of chips)ch.addEventListener('click',()=>{cat=ch.dataset.cat;for(const x of chips)x.setAttribute('aria-pressed',x===ch);f()});
