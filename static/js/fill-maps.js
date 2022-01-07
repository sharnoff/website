// Sets the map contents from the tagged information in the HTML

const MAP_DOM_ID = 'leaflet-map'
const STORAGE_PREFIX_ZOOM = 'map-zoom#'
const STORAGE_PREFIX_COORDS = 'map-coords#'
const STORAGE_PREFIX_ID = 'map-id#'

window.addEventListener('load', (event) => {
    let { mapFrame, photos, config } = JSON.parse(document.getElementById(MAP_DOM_ID).getAttribute('data-map'))

    // We want to check if the previous exact map used for this page is the same; if it is, the
    // coordinates should be exactly re-used.
    let idName = STORAGE_PREFIX_ID + config.name
    let previousMap = window.sessionStorage.getItem(idName)
    window.sessionStorage.setItem(idName, config.id)

    // Make sure that reloading the same page will keep the map as-is.
    let storedCoordsName = STORAGE_PREFIX_COORDS + config.name

    console.log({ previousMap, id: config.id })

    if (previousMap === config.id) {
        let storedCoords = window.sessionStorage.getItem(storedCoordsName)
        console.log({ storedCoords })
        if (storedCoords) {
            mapFrame.centeredAt = JSON.parse(storedCoords)
        }
    }

    // Check if we have a zoom level already specified in the session storage
    let storedZoomName = STORAGE_PREFIX_ZOOM + config.name
    let storedZoom = window.sessionStorage.getItem(storedZoomName)
    if (storedZoom) {
        mapFrame.zoomLevel = parseInt(storedZoom)
    }

    // Initialize the map, using the information from mapFrame and fill it with the points from
    // photos.
    // let coords = [mapFrame.centeredAt.lat, mapFrame.centeredAt.lon]
    let map = L.map(MAP_DOM_ID).setView(mapFrame.centeredAt, mapFrame.zoomLevel)

    map.on('zoomend', (e) => {
        window.sessionStorage.setItem(storedZoomName, map.getZoom().toString())
    })
    map.on('moveend', (e) => {
        let c = map.getCenter()
        window.sessionStorage.setItem(storedCoordsName, JSON.stringify({ lat: c.lat, lon: c.lng }))
    })

    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
        attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors',
    }).addTo(map)

    for (let p of photos) {
        if (!p.coords) continue

        let popupText =
            '<div class="map-popup">'
                + `<div class="photo-title"><a href="/photos/view/${p.file_name}">${p.title}</a></div>`
                + `<div class="photo-date">${p.date}</div>`
            + '</div>'

        let popup = L.marker([p.coords.lat, p.coords.lon])
            .addTo(map)
            .bindPopup(popupText, { autoPan: false, closeOnClick: true, autoClose: false })

        if (photos.length !== 1) popup.openPopup()
    }
})
