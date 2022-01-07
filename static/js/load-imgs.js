// Loads images from a #flex-grid's data its content

// Helper function to give us a convenient API for building HTML elements
//
// It's no jQuery, but oh boy does it help.
//
// For usage examples, see `makeItem`.
function buildElement(tagName, attrs, children) {
    let e = document.createElement(tagName)

    for (const [key, value] of Object.entries(attrs)) {
        e.setAttribute(key, value)
    }

    for (const c of children) {
        e.appendChild(c)
    }

    return e
}

function makeItem(photoInfo, album) {
    let href = `/photos/view/${photoInfo.file_name}`
    if (album) href += `?album=${album}`

    let element = buildElement('div', { class: "photo-smallbox" }, [
        buildElement( 'a', { href }, [
            buildElement('img', {
                src: `/photos/img-file/${photoInfo.file_name}?size=small&rev=${photoInfo.smaller.hash}`,
                alt: photoInfo.alt_text,
            }, []),
            buildElement('div', { class: "photo-overlay" }, [
                buildElement('div', { class: 'photo-caption' }, [
                    buildElement('div', { class: 'photo-date' }, [
                        document.createTextNode(photoInfo.date)
                    ]),
                    buildElement('div', { class: 'photo-title' }, [
                        document.createTextNode(photoInfo.title)
                    ]),
                ])
            ])
        ])
    ])

    let dims = { height: photoInfo.smaller.height, width: photoInfo.smaller.width }

    return { element, dims }
}

window.addEventListener('load', (event) => {
    let container = document.getElementById('flex-grid')

    let { settings, photos, album } = JSON.parse(container.getAttribute('data-imgs'))

    let items = photos.map(p => makeItem(p, album))

    let slider = container.querySelector('input')

    let previousWidth = window.localStorage.getItem('flexGridColumnWidth')
    if (previousWidth) {
        slider.value = previousWidth
        settings.minColumnWidth = parseInt(previousWidth)
    }

    let grid = new FlexGrid(container, items, settings)
    for (const it of items) {
        container.appendChild(it.element)
    }

    window.onresize = () => { grid.rescale() }

    slider.oninput = function () {
        let value = this.value
        grid.setMinColumnWidth(parseInt(value))
        window.localStorage.setItem('flexGridColumnWidth', value)
    }
})
