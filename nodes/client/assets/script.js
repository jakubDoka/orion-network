function adjustHeight(el) {
        var outerHeight = parseInt(window.getComputedStyle(el).height, 10);
        var diff = outerHeight - el.clientHeight;
        el.style.height = 0;
        el.style.height = (el.scrollHeight + diff) + 'px';
}
